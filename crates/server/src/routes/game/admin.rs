use std::collections::{HashMap, HashSet};

use axum::{
  Extension, Json,
  extract::{Query, State},
  response::IntoResponse,
};
use chrono::{DateTime, Utc, serde::ts_seconds};
use r2s_cache::Cache;
use r2s_database::{
  audit, challenge, challenge_milestone, extra, game, submission,
  team::{self, TeamScoreHistory, TeamScoreHistoryList},
  user,
};
use r2s_event::EventManager;
use r2s_migrator::Database;
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};

use crate::{
  traits::ResponseError,
  utility::pagination::{DEFAULT_PAGE_SIZE, page, page_size},
};

#[derive(Serialize)]
pub(super) struct ConnectedDevice {
  client: String,
  address: String,
  #[serde(with = "ts_seconds")]
  connected_at: DateTime<Utc>,
}

pub(super) async fn get_connected_devices(
  State(event): State<EventManager>, Extension(game): Extension<game::Model>,
) -> Result<impl IntoResponse, ResponseError> {
  let clients = event.clients.read().await;
  let clients = clients
    .iter()
    .filter(|(id, ..)| game.id == *id)
    .map(|(_, c, a, d)| ConnectedDevice {
      client: c.clone(),
      address: a.to_string(),
      connected_at: *d,
    })
    .collect::<Vec<_>>();
  Ok(Json(clients))
}

pub(super) async fn regenerate_game_token(
  State(ref db): State<Database>, State(ref cache): State<Cache>,
  Extension(game): Extension<game::Model>,
) -> Result<impl IntoResponse, ResponseError> {
  let token = game::update(
    &db.conn,
    game::Model {
      id: game.id,
      token: Some(nanoid::nanoid!()),
      ..game.clone()
    },
  )
  .await?;
  cache.at("game").del(game.id).await?;
  Ok(Json(token))
}

pub(super) async fn get_game_administrator(
  State(ref db): State<Database>, Extension(game): Extension<game::Model>,
) -> Result<impl IntoResponse, ResponseError> {
  let admins = user::get_multiple(&db.conn, &game.admins.0).await?;
  Ok(Json(admins))
}

pub(super) async fn update_game_administrator(
  State(ref db): State<Database>, State(ref cache): State<Cache>,
  Extension(game): Extension<game::Model>, Json(admins): Json<Vec<i64>>,
) -> Result<impl IntoResponse, ResponseError> {
  let model = game::update(
    &db.conn,
    game::Model {
      id: game.id,
      admins: game::Admins(admins),
      ..game.clone()
    },
  )
  .await?;
  cache.at("game").del(game.id).await?;
  Ok(Json(model))
}

#[derive(Deserialize)]
pub(super) struct PaginateQuery {
  page: Option<u64>,
  page_size: Option<u64>,
}

pub(super) async fn get_submissions(
  State(ref db): State<Database>, Extension(game): Extension<game::Model>,
  Query(query): Query<PaginateQuery>,
) -> Result<impl IntoResponse, ResponseError> {
  let submissions = submission::get_page_ex(
    &db.conn,
    page(query.page),
    page_size(query.page_size, DEFAULT_PAGE_SIZE),
    false,
    true,
    Some(game.id),
    None,
    None,
    None,
  )
  .await?;
  Ok(Json(submissions))
}

#[derive(Debug, Deserialize)]
pub(super) struct RecalculateScoreboardRequest {
  pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct RecalculateScoreboardResponse {
  pub dry_run: bool,
  pub challenges: u64,
  pub blood_extras: u64,
  pub changed_extras: u64,
  pub changed_teams: u64,
  pub score_delta: i64,
}

pub(super) async fn recalculate_scoreboard(
  State(ref db): State<Database>, Extension(game): Extension<game::Model>,
  Json(request): Json<RecalculateScoreboardRequest>,
) -> Result<impl IntoResponse, ResponseError> {
  let txn = db.conn.begin().await?;
  let challenges = challenge::get_full_list(&txn, game.id).await?;
  let challenge_by_id: HashMap<_, _> = challenges.iter().map(|c| (c.id, c)).collect();
  let submissions = submission::get_all_solved_for_game(&txn, game.id).await?;
  let mut first_solves = HashSet::new();
  let mut solve_counts = HashMap::<i64, u64>::new();
  let mut blood_solves = Vec::new();
  let mut team_solves = HashMap::<i64, Vec<(DateTime<Utc>, i64, i64, i32)>>::new();
  for submission in submissions {
    let Some(team_id) = submission.team_id else {
      continue;
    };
    if !first_solves.insert((team_id, submission.challenge_id)) {
      continue;
    }
    team_solves.entry(team_id).or_default().push((
      submission.created_at,
      submission.id,
      submission.challenge_id,
      submission.score,
    ));
    let count = solve_counts.entry(submission.challenge_id).or_default();
    *count += 1;
    if *count <= 3 {
      blood_solves.push((team_id, submission.challenge_id, *count, submission.score));
    }
  }

  let mut extras_by_key = HashMap::new();
  for team in team::get_list_by_game_id(&txn, game.id).await? {
    for extra in extra::get_list(&txn, team.id).await? {
      if let (Some(challenge_id), Some(blood_state)) = (
        extra.challenge_id,
        extra.reason.strip_prefix("No.").and_then(|reason| {
          reason
            .split_once(' ')
            .and_then(|(state, _)| state.parse::<u64>().ok())
        }),
      ) {
        extras_by_key.insert((team.id, challenge_id, blood_state), extra);
      }
    }
  }

  let blood_extras = blood_solves.len() as u64;
  let mut changed_extras = 0;
  let mut score_delta = 0;
  let mut team_deltas = HashMap::<i64, i32>::new();
  let mut blood_scores = HashMap::<(i64, i64), (i32, i32)>::new();
  for (team_id, challenge_id, blood_state, _) in blood_solves {
    let Some(challenge) = challenge_by_id.get(&challenge_id) else {
      continue;
    };
    let award_rate = get_award_rate(&game, blood_state as i32);
    let score = challenge::score_for_decay(&challenge.score_rule, blood_state) * award_rate / 100;
    blood_scores.insert((team_id, challenge_id), (blood_state as i32, score));
    let key = (team_id, challenge_id, blood_state);
    if let Some(extra) = extras_by_key.get(&key) {
      if extra.score != score {
        changed_extras += 1;
        score_delta += i64::from(score - extra.score);
        *team_deltas.entry(team_id).or_default() += score - extra.score;
        if !request.dry_run {
          extra::update(
            &txn,
            extra::Model {
              score,
              ..extra.clone()
            },
          )
          .await?;
        }
      }
    } else if score > 0 {
      changed_extras += 1;
      score_delta += i64::from(score);
      *team_deltas.entry(team_id).or_default() += score;
      if !request.dry_run {
        extra::create(
          &txn,
          extra::Model {
            id: 0,
            created_at: chrono::Utc::now(),
            reason: format!(
              "No.{blood_state} solution for challenge {}:{}",
              challenge.id, challenge.name
            ),
            score,
            hint_id: None,
            team_id,
            challenge_id: Some(challenge_id),
          },
        )
        .await?;
      }
    }
  }

  let mut changed_teams = 0;
  if !request.dry_run {
    let milestones = challenge_milestone::get_list(&txn, game.id).await?;
    for team in team::get_list_by_game_id(&txn, game.id).await? {
      let score = team::calc_score(&txn, team.id).await?;
      let mut history = vec![TeamScoreHistory {
        changed_at: game.start_at,
        score: 0,
        challenge_id: None,
        blood_state: None,
      }];
      let mut solved = HashSet::new();
      let mut previous_milestone_score = 0;
      let mut running_score = 0;
      let mut solves = team_solves.remove(&team.id).unwrap_or_default();
      solves.sort_by_key(|(created_at, id, ..)| (*created_at, *id));
      for (changed_at, _, challenge_id, submission_score) in solves {
        running_score += submission_score;
        let blood = blood_scores.get(&(team.id, challenge_id)).copied();
        if let Some((_, blood_score)) = blood {
          running_score += blood_score;
        }
        solved.insert(challenge_id);
        let milestone_score = challenge_milestone::milestone_bonus(&solved, &milestones);
        running_score += milestone_score - previous_milestone_score;
        previous_milestone_score = milestone_score;
        history.push(TeamScoreHistory {
          changed_at,
          score: running_score,
          challenge_id: Some(challenge_id),
          blood_state: blood.map(|(state, _)| state),
        });
      }
      let last_active_at = history
        .last()
        .filter(|history| history.challenge_id.is_some())
        .map(|history| history.changed_at)
        .unwrap_or(team.last_active_at);
      if score != team.score || team.history.0 != history || team.last_active_at != last_active_at {
        changed_teams += 1;
        team::update(
          &txn,
          team::Model {
            score,
            history: TeamScoreHistoryList(history),
            last_active_at,
            ..team
          },
        )
        .await?;
      }
    }
    txn.commit().await?;
  } else {
    for team in team::get_list_by_game_id(&txn, game.id).await? {
      if team_deltas.get(&team.id).copied().unwrap_or_default() != 0 {
        changed_teams += 1;
      }
    }
  }
  Ok(Json(RecalculateScoreboardResponse {
    dry_run: request.dry_run,
    challenges: challenges.len() as u64,
    blood_extras,
    changed_extras,
    changed_teams,
    score_delta,
  }))
}

fn get_award_rate(game: &game::Model, blood_state: i32) -> i32 {
  if let Some(award_rates) = &game.award_rates {
    return award_rates
      .0
      .get(blood_state as usize - 1)
      .copied()
      .unwrap_or_default();
  }
  if (1..=3).contains(&blood_state) {
    game.award_rate * (4 - blood_state) / 3
  } else {
    0
  }
}

pub(super) async fn get_audit_messages(
  State(ref db): State<Database>, Extension(game): Extension<game::Model>,
  Query(query): Query<PaginateQuery>,
) -> Result<impl IntoResponse, ResponseError> {
  let submissions = audit::get_page_ex(
    &db.conn,
    page(query.page),
    page_size(query.page_size, DEFAULT_PAGE_SIZE),
    Some(game.id),
    None,
    None,
    None,
    None,
  )
  .await?;
  Ok(Json(submissions))
}

pub(super) async fn update_audit(
  State(ref db): State<Database>, Extension(game): Extension<game::Model>,
  Extension(prev_model): Extension<audit::Model>, Json(model): Json<audit::Model>,
) -> Result<impl IntoResponse, ResponseError> {
  if game.id != prev_model.game_id {
    return Err(ResponseError::NotFound("audit not found".to_owned()));
  }
  let model = audit::update(
    &db.conn,
    prev_model.id,
    audit::Model {
      id: prev_model.id,
      challenge_id: prev_model.challenge_id,
      team_id: prev_model.team_id,
      user_id: prev_model.user_id,
      game_id: prev_model.game_id,
      ..model
    },
  )
  .await?;
  Ok(Json(model))
}
