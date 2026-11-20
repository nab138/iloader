import "./Certificates.css";
import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { useStore } from "../StoreContext";
import { useError } from "../ErrorContext";
import { useTranslation } from "react-i18next";

type AppId = {
  appIdId: string;
  identifier: string;
  name: string;
  features: Record<string, any>;
  expirationDate: string | null;
};

type AppIdsResponse = {
  appIds: AppId[];
  maxQuantity: number;
  availableQuantity: number;
};

export const AppIds = () => {
  const { t } = useTranslation();
  const [appIds, setAppIds] = useState<AppId[]>([]);
  const [maxQuantity, setMaxQuantity] = useState<number | null>(null);
  const [availableQuantity, setAvailableQuantity] = useState<number | null>(
    null,
  );
  const [loading, setLoading] = useState<boolean>(false);
  const loadingRef = useRef<boolean>(false);
  const [appIdDeletion] = useStore<boolean>("allowAppIdDeletion", false);

  const { err } = useError();

  const loadAppIds = useCallback(async () => {
    if (loadingRef.current) return;
    const promise = async () => {
      loadingRef.current = true;
      setLoading(true);
      let list = await invoke<AppIdsResponse>("list_app_ids");
      setAppIds(list.appIds);
      setMaxQuantity(list.maxQuantity);
      setAvailableQuantity(list.availableQuantity);
      setLoading(false);
      loadingRef.current = false;
    };
    toast.promise(promise, {
      loading: t("app_ids.loading"),
      success: t("app_ids.loaded_success"),
      error: (e) => err(t("app_ids.failed_load"), e),
    });
  }, [setAppIds, t]);

  const deleteId = useCallback(
    async (id: string) => {
      const promise = invoke<void>("delete_app_id", {
        appIdId: id,
      });
      promise.then(loadAppIds);
      toast.promise(promise, {
        loading: t("apple_id.deleting"),
        success: t("app_ids.deleted_success"),
        error: (e) => err(t("app_ids.failed_delete"), e),
      });
    },
    [setAppIds, loadAppIds, t],
  );

  useEffect(() => {
    loadAppIds();
  }, []);

  return (
    <>
      <h2>{t("app_ids.manage")}</h2>
      {maxQuantity !== null && (
        <div style={{ marginBottom: "0.5em" }}>
          {t("app_ids.available", {
            available: availableQuantity,
            max: maxQuantity,
          })}
        </div>
      )}
      {appIds.length === 0 ? (
        <div>{loading ? t("app_ids.loading") : t("app_ids.none_found")}</div>
      ) : (
        <div className="card">
          <div className="certificate-table-container">
            <table className="certificate-table">
              <thead>
                <tr className="certificate-item">
                  <th className="cert-item-part">{t("app_ids.name")}</th>
                  <th className="cert-item-part">{t("app_ids.expiration")}</th>
                  <th className="cert-item-part">{t("app_ids.id")}</th>
                  <th
                    className="cert-item-part"
                    style={{
                      borderRight: appIdDeletion ? undefined : "none",
                    }}
                  >
                    {t("app_ids.identifier")}
                  </th>
                  {appIdDeletion && <th>{t("common.delete")}</th>}
                </tr>
              </thead>
              <tbody>
                {appIds.map((appId, i) => (
                  <tr
                    key={appId.appIdId}
                    className={
                      "certificate-item" +
                      (i === appIds.length - 1 ? " cert-item-last" : "")
                    }
                  >
                    <td className="cert-item-part">{appId.name}</td>
                    <td className="cert-item-part">
                      {appId.expirationDate
                        ? new Date(appId.expirationDate).toLocaleDateString()
                        : t("app_ids.never")}
                    </td>
                    <td className="cert-item-part">{appId.appIdId}</td>
                    <td
                      className="cert-item-part"
                      style={{
                        borderRight: appIdDeletion ? undefined : "none",
                      }}
                    >
                      {appId.identifier}
                    </td>
                    {appIdDeletion && (
                      <td
                        className="cert-item-revoke"
                        onClick={() => deleteId(appId.appIdId)}
                      >
                        {t("common.delete")}
                      </td>
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
      <button
        style={{ marginTop: "1em", width: "100%" }}
        onClick={loadAppIds}
        disabled={loading}
      >
        {t("common.refresh")}
      </button>
    </>
  );
};
