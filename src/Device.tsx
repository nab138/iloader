import { useCallback, useEffect, useRef, useState } from "react";
import "./Device.css";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { Modal } from "./components/Modal";
import { useError } from "./ErrorContext";
import { AppError } from "./errors";
import { usePlatform } from "./PlatformContext";

export type DeviceInfo = {
  name: string;
  id: number;
  uuid: string;
  connectionType: "USB" | "Network" | "Unknown" | "Wireless";
  version: string;
  deviceClass?: string;
  productType?: string;
  /// Transport used to reach the device. "vision" = Apple Vision Pro over an RP
  /// tunnel (Wi-Fi, no usbmux); otherwise a usbmux iPhone/iPad.
  transport?: "usbmux" | "vision";
  /// Vision Pro IP address (only set for transport === "vision").
  ip?: string;
  /// For a Vision Pro: whether a reusable pairing file is already stored, so it can
  /// be selected directly instead of prompting for the headset code.
  paired?: boolean;
};

/// True when a device must be reached over the Vision Pro RP tunnel rather than
/// usbmux. (A Vision Pro tethered over USB with a dev strap stays on the usbmux path.)
const isVisionTransport = (device: DeviceInfo): boolean =>
  device.transport === "vision";

// Human-friendly device family derived from the lockdown DeviceClass /
// ProductType. Lets an Apple Vision Pro ("RealityDevice" / "RealityDevice17,1")
// be recognised and labelled instead of shown as a generic idevice.
export const deviceFamilyLabel = (device: DeviceInfo): string | null => {
  const cls = device.deviceClass?.toLowerCase() ?? "";
  const product = device.productType?.toLowerCase() ?? "";
  if (cls === "realitydevice" || product.startsWith("realitydevice"))
    return "Apple Vision Pro";
  if (cls === "ipad") return "iPad";
  if (cls === "iphone") return "iPhone";
  if (cls === "ipod") return "iPod touch";
  if (cls === "appletv") return "Apple TV";
  if (cls === "watch") return "Apple Watch";
  return device.deviceClass ?? null;
};

type VisionStatus = null | "connecting" | "awaiting-code" | "verifying" | "paired";

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
  const { platform } = usePlatform();
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  // Why the last refresh found nothing, when mDNS itself couldn't start (e.g. macOS
  // Local Network permission denied / a firewall ate multicast). `null` = discovery
  // is running fine, so the empty state falls back to the generic "check permission"
  // hint rather than a specific error.
  const [discoveryError, setDiscoveryError] = useState<string | null>(null);
  const [waitingToPair, setWaitingToPair] = useState<DeviceInfo | null>(null);
  const [showPairingModal, setShowPairingModal] = useState(false);

  // Vision Pro first-time pairing (headset shows a code; the user types it here).
  const [visionPairing, setVisionPairing] = useState<DeviceInfo | null>(null);
  const [visionStatus, setVisionStatus] = useState<VisionStatus>(null);
  const [visionCode, setVisionCode] = useState("");

  const listingDevices = useRef<boolean>(false);
  const pairingRequestId = useRef<number>(0);
  const pairingModalTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Re-entry guard for Vision Pro pairing, kept out of hook deps so it doesn't
  // churn the identity of selectDevice/loadDevices (which would reload mid-pairing).
  const visionPairingActive = useRef<boolean>(false);

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

  // First-time wireless pairing with a Vision Pro. The headset displays a 6-digit
  // code; the backend blocks until we send it back via the `vision-pair-code` event.
  const startVisionPair = useCallback(
    async (device: DeviceInfo) => {
      if (visionPairingActive.current) return;
      visionPairingActive.current = true;
      setVisionPairing(device);
      setVisionStatus("connecting");
      setVisionCode("");

      const unlisten = await listen<string>("vision-pair-status", (e) =>
        setVisionStatus(e.payload as VisionStatus),
      );

      try {
        await invoke("vision_pair", { device });
        toast.success(t("device.vision_paired", { device: device.name }));
        // The backend has already made this the selected device; reflect it locally
        // and mark the card paired without a full re-list.
        const paired = { ...device, paired: true };
        setDevices((prev) =>
          prev.map((d) => (d.id === device.id ? paired : d)),
        );
        setSelectedDevice(paired);
      } catch (e) {
        const message = String((e as { message?: string })?.message ?? e);
        if (!message.toLowerCase().includes("cancel")) {
          toast.error(err(t("device.vision_pair_failed"), e as AppError));
        }
      } finally {
        unlisten();
        visionPairingActive.current = false;
        setVisionPairing(null);
        setVisionStatus(null);
        setVisionCode("");
      }
    },
    [setSelectedDevice, err, t],
  );

  const submitVisionCode = useCallback(async () => {
    const code = visionCode.replace(/\D/g, "").slice(0, 6);
    if (code.length < 6) return;
    await emit("vision-pair-code", code);
  }, [visionCode]);

  const cancelVisionPair = useCallback(() => {
    invoke("cancel_pairing").catch(() => {});
  }, []);

  const selectDevice = useCallback(
    (device: DeviceInfo | null) => {
      // An unpaired Vision Pro can't be selected silently — it needs the headset
      // code — so route it to the wireless pairing flow instead.
      if (device && isVisionTransport(device) && !device.paired) {
        startVisionPair(device);
        return;
      }

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
    [clearPairingModalTimer, setSelectedDevice, startVisionPair, t],
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
        // Surface a mDNS-startup failure so an empty list isn't mistaken for "no
        // device present" (the common macOS Local Network permission trap).
        invoke<string | null>("vision_discovery_error")
          .then((msg) => setDiscoveryError(msg ?? null))
          .catch(() => setDiscoveryError(null));
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
              // A Vision Pro reports its paired state directly; other devices are
              // checked against the stored RemotePairing cache.
              if (isVisionTransport(device)) {
                return device.paired ? device : null;
              }
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
      <Modal isOpen={visionPairing !== null} close={cancelVisionPair}>
        <div className="pairing-modal-content">
          {visionStatus === "paired" ? (
            <h2>{t("device.vision_paired_short")}</h2>
          ) : visionStatus === "awaiting-code" ? (
            <>
              <h2>{t("device.vision_enter_code")}</h2>
              <p>
                {t("device.vision_enter_code_hint", {
                  device: visionPairing?.name ?? "Vision Pro",
                })}
              </p>
              <input
                autoFocus
                inputMode="numeric"
                pattern="[0-9]*"
                maxLength={6}
                value={visionCode}
                onChange={(e) =>
                  setVisionCode(e.target.value.replace(/\D/g, "").slice(0, 6))
                }
                onKeyDown={(e) => {
                  if (e.key === "Enter") submitVisionCode();
                }}
                placeholder="000000"
                style={{
                  fontSize: 32,
                  fontWeight: 700,
                  letterSpacing: 8,
                  textAlign: "center",
                  width: 240,
                  maxWidth: "100%",
                  padding: "10px 14px",
                  boxSizing: "border-box",
                  margin: "12px 0",
                }}
              />
              <button
                onClick={submitVisionCode}
                disabled={visionCode.replace(/\D/g, "").length < 6}
              >
                {t("device.vision_pair_submit")}
              </button>
            </>
          ) : visionStatus === "verifying" ? (
            <>
              <div className="spinner" />
              <h2>{t("device.vision_verifying")}</h2>
            </>
          ) : (
            <>
              <div className="spinner" />
              <h2>
                {t("device.vision_connecting", {
                  device: visionPairing?.name ?? "Vision Pro",
                })}
              </h2>
              <p>{t("device.vision_connecting_hint")}</p>
            </>
          )}
          <button onClick={cancelVisionPair}>{t("device.pairing_cancel")}</button>
        </div>
      </Modal>
      <h2 style={{ marginTop: 0 }}>{t("device.title")}</h2>
      <div className="credentials-container">
        {devices.length === 0 && (
          <div className="no-devices">
            <div>{t("device.no_devices_found_period")}</div>
            {discoveryError ? (
              <div className="no-devices-hint no-devices-hint--error">
                {t("device.discovery_error_hint", { error: discoveryError })}
              </div>
            ) : platform === "mac" ? (
              <div className="no-devices-hint">
                {t("device.no_devices_hint_mac")}
              </div>
            ) : null}
          </div>
        )}
        {devices.map((device) => {
          const isActive = selectedDevice?.id === device.id;
          const needsPairing =
            isVisionTransport(device) && !device.paired;
          return (
            <button
              key={device.id}
              className={"device-card card" + (isActive ? " active" : "")}
              onClick={() => selectDevice(device)}
              disabled={waitingToPair !== null || visionPairing !== null}
            >
              <div className="device-meta">
                <span className="device-name">{device.name}</span>
                <span className="device-connection">
                  {deviceFamilyLabel(device)
                    ? `${deviceFamilyLabel(device)} · ${device.connectionType}`
                    : device.connectionType}
                </span>
              </div>
              {isActive ? (
                <span className="device-selected-pill">
                  {t("device.selected")}
                </span>
              ) : needsPairing ? (
                <span className="device-selected-pill">
                  {t("device.vision_tap_to_pair")}
                </span>
              ) : null}
            </button>
          );
        })}
        <button
          disabled={waitingToPair !== null || visionPairing !== null}
          onClick={loadDevices}
        >
          {t("common.refresh")}
        </button>
      </div>
    </>
  );
};
