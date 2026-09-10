// Client info sent to Apple's GrandSlam servers as `X-Mme-Client-Info`.
// Format: <hardware model> <os;version;build> <com.apple.AuthKit/1 (<bundle id>/<version>)>
//
// Keep these in sync with `src-tauri/src/anisette.rs`, which holds the same defaults for
// when no value has been stored yet.
export const CLIENT_INFO_PRESETS = [
  {
    value:
      "<Mac15,7> <macOS;27.0;26A5378j> <com.apple.AuthKit/1 (com.apple.akd/1.0)>",
    labelKey: "settings.client_info_akd",
  },
  {
    value:
      "<Mac15,7> <macOS;27.0;26A5378j> <com.apple.AuthKit/1 (com.apple.dt.Xcode/25183.54.10)>",
    labelKey: "settings.client_info_xcode",
  },
];

export const DEFAULT_CLIENT_INFO = CLIENT_INFO_PRESETS[0].value;
