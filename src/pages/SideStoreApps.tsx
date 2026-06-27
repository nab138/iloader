import "./Certificates.css";
import { invoke } from "@tauri-apps/api/core";
import React, { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { useError } from "../ErrorContext";
import { useTranslation } from "react-i18next";
import {
  refreshAllSideStoreAppsOperation,
  refreshSideStoreAppOperation,
  Operation,
  OperationState,
  OperationUpdate,
} from "../components/operations";
import { listen } from "@tauri-apps/api/event";

export type SideStoreApp = {
  name: string;
  bundleId: string;
  baseBundleId: string;
  teamIdentifier?: string | null;
  signerIdentity?: string | null;
  version?: string | null;
  isSideloaded: boolean;
};

type RefreshAllResult = {
  succeeded: string[];
  failed: Record<string, string>;
};

type Props = {
  setOperationState: React.Dispatch<
    React.SetStateAction<OperationState | null>
  >;
};

export const SideStoreApps = ({ setOperationState }: Props) => {
  const { t } = useTranslation();
  const [apps, setApps] = useState<SideStoreApp[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [busyBundle, setBusyBundle] = useState<string | null>(null);
  const loadingRef = useRef<boolean>(false);
  const { err } = useError();

  const loadApps = useCallback(async () => {
    if (loadingRef.current) return;
    const promise = async () => {
      loadingRef.current = true;
      setLoading(true);
      try {
        const list = await invoke<SideStoreApp[]>("list_sidestore_apps");
        setApps(list);
      } finally {
        setLoading(false);
        loadingRef.current = false;
      }
    };
    toast.promise(promise, {
      loading: t("sidestore_apps.loading"),
      success: t("sidestore_apps.loaded_success"),
      error: (e) => err(t("sidestore_apps.failed_load"), e),
    });
  }, [t, err]);

  const startOperation = useCallback(
    async (
      operation: Operation,
      params: Record<string, any>,
    ): Promise<any> => {
      setOperationState({
        current: operation,
        started: [],
        failed: [],
        completed: [],
      });
      return new Promise<any>(async (resolve, reject) => {
        const unlistenFn = await listen<OperationUpdate>(
          "operation_" + operation.id,
          (event) => {
            setOperationState((old) => {
              if (old == null) return null;
              if (event.payload.updateType === "started") {
                return {
                  ...old,
                  started: [...old.started, event.payload.stepId],
                };
              } else if (event.payload.updateType === "finished") {
                return {
                  ...old,
                  completed: [...old.completed, event.payload.stepId],
                };
              } else if (event.payload.updateType === "failed") {
                return {
                  ...old,
                  failed: [
                    ...old.failed,
                    {
                      stepId: event.payload.stepId,
                      extraDetails: event.payload.extraDetails,
                    },
                  ],
                };
              }
              return old;
            });
          },
        );
        try {
          const result = await invoke(operation.id + "_operation", params);
          unlistenFn();
          resolve(result);
        } catch (e) {
          unlistenFn();
          reject(e);
        }
      });
    },
    [setOperationState],
  );

  const refreshOne = useCallback(
    async (app: SideStoreApp) => {
      setBusyBundle(app.bundleId);
      try {
        await startOperation(refreshSideStoreAppOperation, {
          bundleId: app.bundleId,
        });
        toast.success(
          t("sidestore_apps.refreshed_success", { name: app.name }),
        );
      } catch (e: any) {
        toast.error(err(t("sidestore_apps.failed_refresh"), e));
      } finally {
        setBusyBundle(null);
      }
    },
    [startOperation, t, err],
  );

  const refreshAll = useCallback(async () => {
    setBusyBundle("__all__");
    try {
      const result = (await startOperation(
        refreshAllSideStoreAppsOperation,
        {},
      )) as RefreshAllResult;
      const ok = result?.succeeded?.length ?? 0;
      const fail = Object.keys(result?.failed ?? {}).length;
      if (fail === 0) {
        toast.success(
          t("sidestore_apps.refreshed_all_success", { count: ok }),
        );
      } else {
        toast.warning(
          t("sidestore_apps.refreshed_all_partial", {
            ok,
            fail,
          }),
        );
        for (const [bid, msg] of Object.entries(result.failed)) {
          console.error(`Failed to refresh ${bid}: ${msg}`);
        }
      }
    } catch (e: any) {
      toast.error(err(t("sidestore_apps.failed_refresh_all"), e));
    } finally {
      setBusyBundle(null);
    }
  }, [startOperation, t, err]);

  useEffect(() => {
    loadApps();
  }, []);

  return (
    <>
      <h2>{t("sidestore_apps.title")}</h2>
      <p style={{ marginTop: 0, opacity: 0.8 }}>
        {t("sidestore_apps.description")}
      </p>
      {apps.length === 0 ? (
        <div>
          {loading
            ? t("sidestore_apps.loading")
            : t("sidestore_apps.none_found")}
        </div>
      ) : (
        <div className="card">
          <div className="certificate-table-container">
            <table className="certificate-table">
              <thead>
                <tr className="certificate-item">
                  <th className="cert-item-part">
                    {t("sidestore_apps.name")}
                  </th>
                  <th className="cert-item-part">
                    {t("sidestore_apps.bundle_id")}
                  </th>
                  <th className="cert-item-part">
                    {t("sidestore_apps.team_id")}
                  </th>
                  <th className="cert-item-part">
                    {t("sidestore_apps.version")}
                  </th>
                  <th>{t("sidestore_apps.action")}</th>
                </tr>
              </thead>
              <tbody>
                {apps.map((app, i) => (
                  <tr
                    key={app.bundleId}
                    className={
                      "certificate-item" +
                      (i === apps.length - 1 ? " cert-item-last" : "")
                    }
                  >
                    <td className="cert-item-part">{app.name}</td>
                    <td className="cert-item-part">{app.bundleId}</td>
                    <td className="cert-item-part">
                      {app.teamIdentifier ?? "-"}
                    </td>
                    <td className="cert-item-part">{app.version ?? "-"}</td>
                    <td
                      className="pairing-place"
                      role="button"
                      tabIndex={0}
                      onClick={() => {
                        if (busyBundle) return;
                        refreshOne(app);
                      }}
                      style={{
                        opacity:
                          busyBundle && busyBundle !== app.bundleId ? 0.5 : 1,
                        pointerEvents: busyBundle ? "none" : "auto",
                      }}
                    >
                      {busyBundle === app.bundleId
                        ? t("sidestore_apps.refreshing")
                        : t("sidestore_apps.refresh")}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
      <button
        style={{ marginTop: "1em", width: "100%" }}
        onClick={refreshAll}
        disabled={!!busyBundle || apps.length === 0}
      >
        {busyBundle === "__all__"
          ? t("sidestore_apps.refreshing_all")
          : t("sidestore_apps.refresh_all")}
      </button>
      <button
        style={{ marginTop: "1em", width: "100%" }}
        onClick={loadApps}
        disabled={loading || !!busyBundle}
      >
        {t("sidestore_apps.reload")}
      </button>
    </>
  );
};
