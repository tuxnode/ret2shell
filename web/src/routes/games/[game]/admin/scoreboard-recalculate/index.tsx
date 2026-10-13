import { type ScoreboardRecalculation, useRecalculateScoreboardMutation } from "@api/game";
import { useParams } from "@solidjs/router";
import { Title } from "@storage/header";
import { t } from "@storage/theme";
import Button from "@widgets/button";
import Card from "@widgets/card";
import Tag from "@widgets/tag";
import { createMemo, createSignal, Show } from "solid-js";

export default function () {
  const params = useParams();
  const gameId = createMemo(() => Number.parseInt(params.game ?? "", 10) || -1);
  const [result, setResult] = createSignal<ScoreboardRecalculation>();
  const mutation = useRecalculateScoreboardMutation({
    onSuccess: (data) => setResult(data),
  });

  function recalculate(dryRun: boolean) {
    setResult();
    mutation.mutate({ game_id: gameId(), dry_run: dryRun });
  }

  return (
    <>
      <Title page={t("game.scoreboardRecalculate.title")} route={`/games/${gameId()}/admin/scoreboard-recalculate`} />
      <div class="w-full p-3 lg:p-6 flex flex-col flex-1 space-y-2">
        <h3 class="min-h-12 py-2 flex items-center border-b border-b-layer-content/10 font-bold space-x-2">
          <span class="shrink-0 icon-[fluent--arrow-sync-20-regular] w-5 h-5" />
          <span>{t("game.scoreboardRecalculate.title")}</span>
        </h3>
        <Card level="warning" contentClass="p-4 flex flex-col space-y-3">
          <p>{t("game.scoreboardRecalculate.description")}</p>
          <div class="flex flex-row flex-wrap gap-2">
            <Button
              level="info"
              onClick={() => recalculate(true)}
              loading={mutation.isPending}
              disabled={mutation.isPending}
            >
              {t("game.scoreboardRecalculate.preview")}
            </Button>
            <Button
              level="warning"
              onClick={() => recalculate(false)}
              loading={mutation.isPending}
              disabled={mutation.isPending}
            >
              {t("game.scoreboardRecalculate.execute")}
            </Button>
          </div>
        </Card>
        <Show when={result()}>
          {(data) => (
            <Card contentClass="p-4 flex flex-col space-y-2">
              <Tag level={data().dry_run ? "info" : "success"}>
                {data().dry_run
                  ? t("game.scoreboardRecalculate.previewResult")
                  : t("game.scoreboardRecalculate.executeResult")}
              </Tag>
              <p>{t("game.scoreboardRecalculate.challenges", { count: data().challenges })}</p>
              <p>{t("game.scoreboardRecalculate.bloodExtras", { count: data().blood_extras })}</p>
              <p>{t("game.scoreboardRecalculate.changedExtras", { count: data().changed_extras })}</p>
              <p>{t("game.scoreboardRecalculate.changedTeams", { count: data().changed_teams })}</p>
              <p>{t("game.scoreboardRecalculate.scoreDelta", { count: data().score_delta })}</p>
            </Card>
          )}
        </Show>
      </div>
    </>
  );
}
