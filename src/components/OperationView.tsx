import { OperationState } from "./operations";
import "./OperationView.css";
import { Modal } from "./Modal";
import {
  FaCircleExclamation,
  FaCircleCheck,
  FaCircleMinus,
} from "react-icons/fa6";
import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Trans, useTranslation } from "react-i18next";
import { ErrorVariant, getErrorSuggestions, parseLinkToken } from "../errors";
import { useStore } from "../StoreContext";
import { usePlatform } from "../PlatformContext";
// import { useDialog } from "../DialogContext";

export default ({
  operationState,
  closeMenu,
}: {
  operationState: OperationState;
  closeMenu: () => void;
}) => {
  const { t } = useTranslation();
  const operation = operationState.current;
  const definedStepIds = new Set(operation.steps.map((s) => s.id));

  const startedDefined = operationState.started.filter((id) => definedStepIds.has(id));
  const completedDefined = operationState.completed.filter((id) => definedStepIds.has(id));
  const failedDefined = operationState.failed.filter((f) => definedStepIds.has(f.stepId));

  const completedSet = new Set(completedDefined);
  const failedSet = new Set(failedDefined.map((f) => f.stepId));

  const opFailed = operationState.failed.length > 0;
  const done = operation.steps.every(
    (step) => completedSet.has(step.id) || failedSet.has(step.id),
  );
  const canDismiss = done || opFailed;

  const detailStepIds =
    operation.id === "install_sidestore"
      ? ["download", "prepare", "sign", "transfer", "install", "pairing"]
      : operation.id === "sideload"
        ? ["prepare", "sign", "transfer", "install"]
        : operation.steps.map((s) => s.id);

  const detailStartedSet = new Set(
    operationState.started.filter((id) => detailStepIds.includes(id)),
  );
  const detailCompletedSet = new Set(
    operationState.completed.filter((id) => detailStepIds.includes(id)),
  );
  const detailFailedSet = new Set(
    operationState.failed
      .map((f) => f.stepId)
      .filter((id) => detailStepIds.includes(id)),
  );

  const defaultStepWeight = 1;
  const detailedStepWeights: Record<string, number> = {
    download: 4,
    prepare: 0.5,
    sign: 5,
    transfer: 5,
    install: 2,
    pairing: 0.5,
    cert: 1,
    profile: 1,
  };

  const getStepWeight = (stepId: string) =>
    operation.id === "sideload" || operation.id === "install_sidestore"
      ? (detailedStepWeights[stepId] ?? defaultStepWeight)
      : defaultStepWeight;

  const totalWeight = detailStepIds.reduce(
    (sum, stepId) => sum + getStepWeight(stepId),
    0,
  );

  const weightedProgress = detailStepIds.reduce((sum, stepId) => {
    const weight = getStepWeight(stepId);
    const failed = detailFailedSet.has(stepId);
    const completed = detailCompletedSet.has(stepId);
    const started = detailStartedSet.has(stepId);
    const reported = operationState.progress[stepId] ?? 0;

    if (completed) return sum + weight;

    if (failed) {
      const failedProgress = started
        ? Math.max(0.02, Math.min(0.98, reported))
        : Math.max(0, Math.min(0.98, reported));
      return sum + weight * failedProgress;
    }

    if (started) return sum + weight * Math.max(0.02, Math.min(0.98, reported));
    return sum;
  }, 0);

  const progressPercent =
    totalWeight > 0
      ? (() => {
          const ratio = ((done && !opFailed ? totalWeight : weightedProgress) / totalWeight);
          const raw = Math.min(100, Math.round(ratio * 100));
          return opFailed ? Math.min(99, raw) : raw;
        })()
      : 0;

  const formatBytes = (bytes: number) => {
    if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
    if (bytes < 1024) return `${Math.floor(bytes)} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  };

  const failedDetailStepIdsInOrder = detailStepIds.filter((id) =>
    operationState.failed.some((f) => f.stepId === id),
  );

  // Prefer the earliest failed detailed step so wrapper failures (e.g. "install")
  // don't override the true failing inner step (e.g. "sign").
  const pinnedFailedDetailStepId =
    failedDetailStepIdsInOrder.find((id) => !detailCompletedSet.has(id)) ??
    failedDetailStepIdsInOrder[0] ??
    null;

  const detailCurrentStepId = done && !opFailed
    ? null
    : pinnedFailedDetailStepId ??
      detailStepIds.find((id) => detailStartedSet.has(id) && !detailCompletedSet.has(id)) ??
      detailStepIds.find((id) => !detailCompletedSet.has(id)) ??
      null;

  const getDetailStepTitle = (stepId: string) => {
    if (operation.id === "install_sidestore" && stepId === "install") {
      return t("operations.sideload_step_install");
    }

    const fromOperation = operation.steps.find((s) => s.id === stepId);
    if (fromOperation) return t(fromOperation.titleKey);

    if (stepId === "prepare") return t("operations.sideload_step_prepare");
    if (stepId === "sign") return t("operations.sideload_step_sign");
    if (stepId === "transfer") return t("operations.sideload_step_transfer");

    return stepId;
  };

  const currentStepTransferInfo = detailCurrentStepId
    ? operationState.transferBytes[detailCurrentStepId]
    : undefined;

  const byteProgressStepIds = new Set(["download", "transfer"]);
  const shouldShowByteProgress =
    detailCurrentStepId !== null && byteProgressStepIds.has(detailCurrentStepId);

  const hasValidByteProgress =
    shouldShowByteProgress &&
    currentStepTransferInfo !== undefined &&
    Number.isFinite(currentStepTransferInfo.uploaded) &&
    Number.isFinite(currentStepTransferInfo.total) &&
    currentStepTransferInfo.total > 0 &&
    currentStepTransferInfo.uploaded > 0;

  const displayStepText =
    detailCurrentStepId && hasValidByteProgress
      ? `${getDetailStepTitle(detailCurrentStepId)} (${formatBytes(currentStepTransferInfo!.uploaded)}/${formatBytes(currentStepTransferInfo!.total)})`
      : detailCurrentStepId
        ? getDetailStepTitle(detailCurrentStepId)
        : done && !opFailed
          ? t("operation.completed")
          : t("operation.preparing");

  const [moreDetailsOpen, setMoreDetailsOpen] = useState(false);
  const [anisetteServer] = useStore<string>(
    "anisetteServer",
    "ani.sidestore.io",
  );
  const { platform } = usePlatform();
  // const { confirm } = useDialog();
  const [suggestions, setSuggestions] = useState<string[]>([]);
  const [underage, setUnderage] = useState<boolean>(false);

  // const handleCancelOperation = () => {
  //   confirm(
  //     t("operation.cancel"),
  //     t("operation.cancel_confirm"),
  //     () => {
  //       closeMenu();
  //     },
  //   );
  // };

  const getSuggestions = useCallback(
    (type: ErrorVariant): string[] => {
      return getErrorSuggestions(t, type, platform, anisetteServer);
    },
    [anisetteServer, t, platform],
  );

  useEffect(() => {
    if (operationState.failed.length > 0) {
      const suggestionSet = new Set<string>();
      for (let f of operationState.failed) {
        if (f.extraDetails.type === "underage") {
          setUnderage(true);
        }
        for (const suggestion of getSuggestions(f.extraDetails.type)) {
          suggestionSet.add(suggestion);
        }
      }
      setSuggestions([...suggestionSet]);
    }
  }, [operationState, getSuggestions]);

  return (
    <Modal
      isOpen={true}
      close={() => {
        if (canDismiss) closeMenu();
      }}
      hideClose={!canDismiss}
      sizeFit
    >
      <div className="operation-header">
        {/*
          Debug-only cancel button.
          in China, Apple ID login can be very slow, so this helps QA quickly dismiss
          the operation modal without restarting and re-logging in.
          Note: this is currently frontend-only behavior; backend cancellation is not wired yet.
        */}
          {/* <button
            className="operation-cancel"
            onClick={handleCancelOperation}
            type="button"
          >
            {t("operation.cancel")}
          </button> */}
        <h2>
          {done && !opFailed && operation.successTitleKey
            ? t(operation.successTitleKey)
            : t(operation.titleKey)}
        </h2>
        <p>
          {done
            ? opFailed
              ? t("operation.failed")
              : t("operation.completed")
            : t("operation.please_wait")}
        </p>
        <div className="operation-progress" aria-hidden={done}>
          <div className="operation-progress-bar">
            <div
              className="operation-progress-fill"
              style={{ width: `${progressPercent}%` }}
            />
          </div>
          <div className="operation-progress-meta">
            <span className="operation-progress-percent">{progressPercent}%</span>
            <span className="operation-progress-step">{displayStepText}</span>
          </div>
        </div>
      </div>
      <div className="operation-content-container">
        <div className="operation-content">
          {operation.steps.map((step) => {
            const stepIds = [step.id];

            const failed = failedDefined.find((f) => stepIds.includes(f.stepId));
            let completed = stepIds.every((id) => completedDefined.includes(id));
            let started = stepIds.some((id) => startedDefined.includes(id));

            if (done && !failed) {
              completed = true;
              started = false;
            }

            const notStarted = !failed && !completed && !started;

            // a little bit gross but it gets the job done.
            let lines =
              failed?.extraDetails.message
                ?.split("\n")
                .filter((line) => line.includes("●")) ?? [];
            let errorShort =
              lines[lines.length - 1]?.replace(/●\s*/, "").trim() ?? "";

            return (
              <div className="operation-step" key={step.id}>
                <div className="operation-step-icon">
                  {failed && (
                    <FaCircleExclamation className="operation-error" />
                  )}
                  {!failed && completed && (
                    <FaCircleCheck className="operation-check" />
                  )}
                  {!failed && !completed && started && (
                    <div className="loading-icon" />
                  )}
                  {notStarted && !opFailed && <div className="waiting-icon" />}
                  {notStarted && opFailed && (
                    <FaCircleMinus className="operation-skipped" />
                  )}
                </div>

                <div className="operation-step-internal">
                  <p>{t(step.titleKey)}</p>
                  {failed && (
                    <>
                      <pre className="operation-extra-details">
                        {!errorShort
                          ? failed.extraDetails.message.replace(/^\n+/, "")
                          : errorShort}
                      </pre>
                      {errorShort !== "" &&
                        errorShort !== null &&
                        errorShort !== undefined && (
                          <>
                            <p
                              className="operation-more-details"
                              role="button"
                              tabIndex={0}
                              onClick={() =>
                                setMoreDetailsOpen(!moreDetailsOpen)
                              }
                            >
                              {t("common.more_details")}{" "}
                              {moreDetailsOpen ? "▲" : "▼"}
                            </p>
                            {moreDetailsOpen && (
                              <pre className="operation-extra-details">
                                {failed.extraDetails.message.replace(
                                  /^\n+/,
                                  "",
                                )}
                              </pre>
                            )}
                          </>
                        )}
                    </>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      </div>
      {done && !opFailed && operation.successMessageKey && (
        <p className="operation-success-message">
          {t(operation.successMessageKey!)}
        </p>
      )}
      {done && !(!opFailed && operation.successMessageKey) && <p></p>}
      {opFailed && (
        <div className="operation-suggestions">
          {suggestions.length > 0 && <h3>{t("error.suggestions_heading")}</h3>}
          {suggestions.length > 0 && (
            <ul>
              {suggestions.map((s) => (
                <li key={s}>
                  {s
                    .split(/(\(\(link:[^)]+\)\)|\(\(link:[^)]+\)\))/g)
                    .map((part, index) => {
                      const parsed = parseLinkToken(part);
                      if (parsed) {
                        const { url, text } = parsed;
                        return (
                          <span
                            key={index}
                            onClick={() => openUrl(url)}
                            role="link"
                            className="error-link"
                          >
                            {text}
                          </span>
                        );
                      }
                      return <span key={index}>{part}</span>;
                    })}
                </li>
              ))}
            </ul>
          )}
          {!underage && (
            <p>
              <Trans
                i18nKey="error.support_message"
                components={{
                  discord: (
                    <span
                      onClick={() => openUrl("https://discord.gg/EA6yVgydBz")}
                      role="link"
                      className="error-link"
                    />
                  ),
                  github: (
                    <span
                      onClick={() =>
                        openUrl("https://github.com/nab138/iloader/issues")
                      }
                      role="link"
                      className="error-link"
                    />
                  ),
                }}
              />
            </p>
          )}
          <button
            style={{ width: "100%" }}
            className="action-button primary"
            onClick={() => {
              navigator.clipboard.writeText(
                "```\n" +
                  (operationState.failed[0]?.extraDetails?.message.replace(
                    /^\n+/,
                    "",
                  ) ?? t("common.no_error")) +
                  "\n```",
              );
              toast.success(t("common.copied_success"));
            }}
          >
            {t("operation.copy_error_clipboard")}
          </button>
        </div>
      )}
      {canDismiss && (
        <button
          style={{ width: "100%" }}
          className="operation-dismiss"
          onClick={closeMenu}
        >
          {t("common.dismiss")}
        </button>
      )}
    </Modal>
  );
};
