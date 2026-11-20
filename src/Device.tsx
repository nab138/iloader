import { useCallback, useEffect, useRef, useState } from "react";
import "./Device.css";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { Modal } from "./components/Modal";
import { useError } from "./ErrorContext";
import { AppError } from "./errors";

export type DeviceInfo = {
  name: string;
  id: number;
  uuid: string;
  connectionType: "USB" | "Network" | "Unknown";
  version: string;
};

export const Device = ({
  selectedDevice,
  setSelectedDevice,
  registerRefresh,
}: {
  selectedDevice: DeviceInfo | null;
  setSelectedDevice: (device: DeviceInfo | null) => void;
  registerRefresh?: (fn?: () => void) => void;
}) => {
  const { t } = useTranslation();
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [waitingToPair, setWaitingToPair] = useState<DeviceInfo | null>(null);
  const [showPairingModal, setShowPairingModal] = useState(false);

  const listingDevices = useRef<boolean>(false);
  const pairingRequestId = useRef<number>(0);
  const pairingModalTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const { err } = useError();

  const clearPairingModalTimer = useCallback(() => {
    if (pairingModalTimer.current) {
      clearTimeout(pairingModalTimer.current);
      pairingModalTimer.current = null;
    }
  }, []);

  useEffect(() => {
    return () => {
      clearPairingModalTimer();
    };
  }, [clearPairingModalTimer]);

  const selectDevice = useCallback(
    (device: DeviceInfo | null) => {
      const requestId = ++pairingRequestId.current;
      clearPairingModalTimer();
      setShowPairingModal(false);
      setWaitingToPair(device);

      if (device) {
        pairingModalTimer.current = setTimeout(() => {
          if (pairingRequestId.current === requestId) {
            setShowPairingModal(true);
          }
        }, 100);
      }

      invoke("set_selected_device", { device })
        .then(() => {
          if (pairingRequestId.current !== requestId) {
            return;
          }
          clearPairingModalTimer();
          setShowPairingModal(false);
          setWaitingToPair(null);
          setSelectedDevice(device);
        })
        .catch((e) => {
          if (pairingRequestId.current !== requestId) {
            return;
          }

          const message = String((e.message ?? e) ?? "Unknown error");
          if (message !== "Pairing cancelled") {
            toast.error(err(t("device.failed_select"), e));
          }
          clearPairingModalTimer();
          setShowPairingModal(false);
          setWaitingToPair(null);
        });
    },
    [clearPairingModalTimer, setSelectedDevice, t],
  );

  const loadDevices = useCallback(async () => {
    if (listingDevices.current) return;
    const promise = new Promise<number>(async (resolve, reject) => {
      listingDevices.current = true;
      try {
        const results = await invoke<
          Array<{ Ok: DeviceInfo } | { Err: AppError }>
        >("list_devices");

        const devices: DeviceInfo[] = [];
        for (const result of results) {
          if ("Ok" in result) {
            devices.push(result.Ok);
          } else if ("Err" in result) {
            toast.error(err(t("device.unable_load_devices_prefix"), result.Err));
          }
        }

        setDevices(devices);
        if (selectedDevice) {
          const stillAvailable = devices.find(
            (d) => d.id === selectedDevice.id,
          );
          if (!stillAvailable) {
            selectDevice(null);
          }
        }
        if (devices.length > 0) {
          const devicesWithPairing = await Promise.all(
            devices.map(async (device) => {
              const hasPairing = await invoke<boolean>("has_stored_rppairing", {
                device,
              });
              return hasPairing ? device : null;
            }),
          )
            .catch(() => [])
            .then((results) =>
              results.filter((d): d is DeviceInfo => d !== null),
            );
          if (devicesWithPairing.length > 0) {
            selectDevice(devicesWithPairing[0]);
          }
        }
        listingDevices.current = false;
        resolve(devices.length);
      } catch (e) {
        setDevices([]);
        selectDevice(null);
        listingDevices.current = false;
        reject(e);
      }
    });

    toast.promise(promise, {
      loading: t("device.loading_devices"),
      success: (count) => {
        if (count === 0) {
          return t("device.no_devices_found");
        }
        return count > 1 ? t("device.found_devices") : t("device.found_device");
      },
      error: (e) => err(t("device.unable_load_devices_prefix"), e),
    });
  }, [setDevices, selectDevice, t]);
  useEffect(() => {
    loadDevices();
  }, [loadDevices]);

  useEffect(() => {
    registerRefresh?.(loadDevices);
    return () => registerRefresh?.(undefined);
  }, [registerRefresh, loadDevices]);

  return (
    <>
      <Modal
        isOpen={showPairingModal && waitingToPair !== null}
        close={() => {
          pairingRequestId.current += 1;
          clearPairingModalTimer();
          setShowPairingModal(false);
          invoke("cancel_pairing").catch(() => { });
          setWaitingToPair(null);
        }}
      >
        <div className="pairing-modal-content">
          <div className="spinner" />
          <h2>
            {t("device.pairing_in_progress_header", {
              device: waitingToPair?.name ?? "Unknown Device",
            })}
          </h2>
          <p>{t("device.pairing_in_progress_hint")}</p>
          <button
            onClick={async () => {
              pairingRequestId.current += 1;
              clearPairingModalTimer();
              setShowPairingModal(false);
              await invoke("cancel_pairing");
              setWaitingToPair(null);
            }}
          >
            {t("device.pairing_cancel")}
          </button>
        </div>
      </Modal>
      <h2 style={{ marginTop: 0 }}>{t("device.title")}</h2>
      <div className="credentials-container">
        {devices.length === 0 && (
          <div>{t("device.no_devices_found_period")}</div>
        )}
        {devices.map((device) => {
          const isActive = selectedDevice?.id === device.id;
          return (
            <button
              key={device.id}
              className={"device-card card" + (isActive ? " active" : "")}
              onClick={() => selectDevice(device)}
              disabled={waitingToPair !== null}
            >
              <div className="device-meta">
                <span className="device-name">{device.name}</span>
                <span className="device-connection">
                  {device.connectionType}
                </span>
              </div>
              {isActive && (
                <span className="device-selected-pill">
                  {t("device.selected")}
                </span>
              )}
            </button>
          );
        })}
        <button disabled={waitingToPair !== null} onClick={loadDevices}>
          {t("common.refresh")}
        </button>
      </div>
    </>
  );
};
