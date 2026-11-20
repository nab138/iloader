import "./Certificates.css";
import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { useError } from "../ErrorContext";
import { useDialog } from "../DialogContext";
import { useTranslation } from "react-i18next";

type PairingAppInfo = {
  name: string;
  bundleId: string;
  path: string;
};

export const Pairing = () => {
  const { t } = useTranslation();
  const [apps, setApps] = useState<PairingAppInfo[]>([]);

  const [loading, setLoading] = useState<boolean>(false);
  const [failed, setFailed] = useState<boolean>(false);
  const loadingRef = useRef<boolean>(false);
  const { err } = useError();
  const { confirm } = useDialog();

  const loadApps = useCallback(async () => {
    if (loadingRef.current) return;
    const promise = async () => {
      loadingRef.current = true;
      setLoading(true);
      try {
        let list = await invoke<PairingAppInfo[]>("installed_pairing_apps");
        setApps(list);
      } catch (e) {
        setFailed(true);
        throw e;
      } finally {
        setFailed(false);
        setLoading(false);
        loadingRef.current = false;
      }
    };
    toast.promise(promise, {
      loading: t("pairing.loading_apps"),
      success: t("pairing.apps_loaded_success"),
      error: (e) => err(t("pairing.failed_load_apps"), e),
    });
  }, [setApps, t]);

  const pair = useCallback(
    async (app: PairingAppInfo) => {
      const promise = invoke<void>("place_pairing_cmd", {
        bundleId: app.bundleId,
        path: app.path,
      });
      toast.promise(promise, {
        loading: t("pairing.placing_pairing_file"),
        success: t("pairing.pairing_file_placed_success"),
        error: (e) => err(t("pairing.failed_place_pairing"), e),
      });
    },
    [setApps, loadApps, t],
  );

  useEffect(() => {
    loadApps();
  }, []);

  return (
    <>
      <h2>{t("pairing.manage")}</h2>
      {apps.length === 0 ? (
        <div>
          {loading
            ? t("pairing.loading_app")
            : failed
              ? t("pairing.failed_load_apps")
              : t("pairing.no_supported_apps_found")}
        </div>
      ) : (
        <div className="card">
          <div className="certificate-table-container">
            <table className="certificate-table">
              <thead>
                <tr className="certificate-item">
                  <th className="cert-item-part">{t("pairing.name")}</th>
                  <th className="cert-item-part">{t("pairing.bundle_id")}</th>
                  <th>{t("pairing.place_pairing_file")}</th>
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
                    <td
                      className="pairing-place"
                      onClick={() => pair(app)}
                      role="button"
                      tabIndex={0}
                    >
                      {t("pairing.place")}
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
        onClick={() => {
          let promise = async () => {
            for (const app of apps) {
              await invoke<void>("place_pairing_cmd", {
                bundleId: app.bundleId,
                path: app.path,
              });
            }
          };
          toast.promise(promise, {
            loading: t("pairing.placing_pairing_file"),
            success: t("pairing.pairing_file_placed_success"),
            error: (e) => err(t("pairing.failed_place_pairing"), e),
          });
        }}
      >
        {t("pairing.place_all")}
      </button>
      <button
        style={{ marginTop: "1em", width: "100%" }}
        onClick={() => {
          confirm(
            t("pairing.advanced_export_title"),
            t("pairing.advanced_export_message"),
            () => {
              const promise = invoke<void>("export_pairing_cmd");
              toast.promise(promise, {
                loading: t("pairing.exporting_pairing_file"),
                success: t("pairing.pairing_file_exported_success"),
                error: (e) => err(t("pairing.failed_export_pairing_file"), e),
              });
            },
          );
        }}
      >
        {t("pairing.export_not_recommended")}
      </button>
      <button
        style={{ marginTop: "1em", width: "100%" }}
        onClick={loadApps}
        disabled={loading}
      >
        {t("pairing.refresh_installed_apps")}
      </button>
    </>
  );
};
