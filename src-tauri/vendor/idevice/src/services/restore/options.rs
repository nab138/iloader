use plist::Value;

/// Builder for the `RestoreOptions` dictionary of an iOS restore.
#[derive(Debug, Clone)]
pub struct RestoreOptions {
    /// Delay (seconds) before the device auto-boots after restore.
    pub auto_boot_delay: i64,
    /// Whether NOR should be flashed.
    pub flash_nor: bool,
    /// Whether the baseband should be updated during restore.
    pub update_baseband: bool,
    /// Whether personalization happens during preflight.
    pub personalized_during_preflight: bool,
    /// Optional boot-args set on the device before restore.
    pub restore_boot_args: Option<String>,
    /// Optional baseband nonce (from firmware preflight info).
    pub baseband_nonce: Option<Vec<u8>>,
    pub bb_updater_state: Option<plist::Dictionary>,
    /// Optional TZ0 required capacity (from the SEP manifest info).
    pub tz0_required_capacity: Option<i64>,
    /// The unique restore session UUID (upper-case).
    pub uuid: String,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            auto_boot_delay: 0,
            flash_nor: true,
            update_baseband: false,
            personalized_during_preflight: true,
            restore_boot_args: None,
            baseband_nonce: None,
            bb_updater_state: None,
            tz0_required_capacity: None,
            uuid: uuid::Uuid::new_v4().to_string().to_uppercase(),
        }
    }
}

impl RestoreOptions {
    /// Creates options with iOS-restore defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serializes the options into the dictionary expected by `StartRestore`.
    pub fn build(&self) -> plist::Dictionary {
        let mut d = plist::Dictionary::new();

        d.insert("AutoBootDelay".into(), self.auto_boot_delay.into());
        d.insert("BootImageType".into(), "UserOrInternal".into());
        d.insert("DFUFileType".into(), "RELEASE".into());
        d.insert("DataImage".into(), false.into());
        d.insert("FirmwareDirectory".into(), ".".into());
        d.insert("FlashNOR".into(), self.flash_nor.into());
        d.insert("KernelCacheType".into(), "Release".into());
        d.insert("NORImageType".into(), "production".into());
        d.insert("RestoreBundlePath".into(), "/tmp/Per2.tmp".into());
        d.insert("SystemImageType".into(), "User".into());
        d.insert("UpdateBaseband".into(), self.update_baseband.into());

        // iOS 18+ additions.
        d.insert("HostHasFixFor99053849".into(), true.into());
        d.insert("SystemImageFormat".into(), "AEAWrappedDiskImage".into());
        d.insert(
            "WaitForDeviceConnectionToFinishStateMachine".into(),
            false.into(),
        );
        d.insert(
            "SupportedAsyncDataTypes".into(),
            Value::Dictionary(supported_async_data_types()),
        );

        d.insert(
            "PersonalizedDuringPreflight".into(),
            self.personalized_during_preflight.into(),
        );

        d.insert("RootToInstall".into(), false.into());
        d.insert("UUID".into(), self.uuid.clone().into());
        d.insert("CreateFilesystemPartitions".into(), true.into());
        d.insert("SystemImage".into(), true.into());
        d.insert(
            "SystemPartitionPadding".into(),
            Value::Dictionary(default_system_partition_padding()),
        );

        d.insert(
            "SupportedDataTypes".into(),
            Value::Dictionary(supported_data_types()),
        );
        d.insert(
            "SupportedMessageTypes".into(),
            Value::Dictionary(supported_message_types()),
        );

        if let Some(args) = &self.restore_boot_args {
            d.insert("RestoreBootArgs".into(), args.clone().into());
        }
        if let Some(nonce) = &self.baseband_nonce {
            d.insert("BasebandNonce".into(), Value::Data(nonce.clone()));
        }
        if let Some(state) = &self.bb_updater_state {
            d.insert("BBUpdaterState".into(), Value::Dictionary(state.clone()));
        }
        if let Some(cap) = self.tz0_required_capacity {
            d.insert("TZ0RequiredCapacity".into(), cap.into());
        }

        d
    }
}

/// The default `SystemPartitionPadding` table.
fn default_system_partition_padding() -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    for (k, v) in [
        ("8", 80i64),
        ("16", 160),
        ("32", 320),
        ("64", 640),
        ("128", 1280),
        ("256", 1280),
        ("512", 1280),
        ("768", 1280),
        ("1024", 1280),
    ] {
        d.insert(k.into(), v.into());
    }
    d
}

fn supported_async_data_types() -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    for (k, v) in [
        ("BasebandData", false),
        ("RecoveryOSASRImage", false),
        ("StreamedImageDecryptionKey", false),
        ("SystemImageData", false),
        ("URLAsset", true),
    ] {
        d.insert(k.into(), v.into());
    }
    d
}

/// The `SupportedDataTypes` table advertised to `restored`.
fn supported_data_types() -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    for (k, v) in SUPPORTED_DATA_TYPES {
        d.insert((*k).into(), (*v).into());
    }
    d
}

/// The `SupportedMessageTypes` table advertised to `restored`.
fn supported_message_types() -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    for (k, v) in SUPPORTED_MESSAGE_TYPES {
        d.insert((*k).into(), (*v).into());
    }
    d
}

/// `(data type, whether it may be sent asynchronously)`.
const SUPPORTED_DATA_TYPES: &[(&str, bool)] = &[
    ("BasebandBootData", false),
    ("BasebandData", false),
    ("BasebandStackData", false),
    ("BasebandUpdaterOutputData", false),
    ("BootabilityBundle", false),
    ("BuildIdentityDict", false),
    ("BuildIdentityDictV2", false),
    ("DataType", false),
    ("DiagData", false),
    ("EANData", false),
    ("FDRMemoryCommit", false),
    ("FDRTrustData", false),
    ("FUDData", false),
    ("FileData", false),
    ("FileDataDone", false),
    ("FirmwareUpdaterData", false),
    ("GrapeFWData", false),
    ("HPMFWData", false),
    ("HostSystemTime", true),
    ("KernelCache", false),
    ("NORData", false),
    ("NitrogenFWData", true),
    ("OpalFWData", false),
    ("OverlayRootDataCount", false),
    ("OverlayRootDataForKey", true),
    ("PeppyFWData", true),
    ("PersonalizedBootObjectV3", false),
    ("PersonalizedData", true),
    ("ProvisioningData", false),
    ("RamdiskFWData", true),
    ("RecoveryOSASRImage", true),
    ("RecoveryOSAppleLogo", true),
    ("RecoveryOSDeviceTree", true),
    ("RecoveryOSFileAssetImage", true),
    ("RecoveryOSIBEC", true),
    ("RecoveryOSIBootFWFilesImages", true),
    ("RecoveryOSImage", true),
    ("RecoveryOSKernelCache", true),
    ("RecoveryOSLocalPolicy", true),
    ("RecoveryOSOverlayRootDataCount", false),
    ("RecoveryOSRootTicketData", true),
    ("RecoveryOSStaticTrustCache", true),
    ("RecoveryOSVersionData", true),
    ("RootData", false),
    ("RootTicket", false),
    ("S3EOverride", false),
    ("SourceBootObjectV3", false),
    ("SourceBootObjectV4", false),
    ("SsoServiceTicket", false),
    ("StockholmPostflight", false),
    ("SystemImageCanonicalMetadata", false),
    ("SystemImageData", false),
    ("SystemImageRootHash", false),
    ("USBCFWData", false),
    ("USBCOverride", false),
    ("FirmwareUpdaterPreflight", true),
    ("ReceiptManifest", true),
    ("FirmwareUpdaterDataV2", false),
    ("RestoreLocalPolicy", true),
    ("AuthInstallCACert", true),
    ("OverlayRootDataForKeyIndex", true),
    ("FirmwareUpdaterDataV3", true),
    ("MessageUseStreamedImageFile", true),
    ("UpdateVolumeOverlayRootDataCount", true),
    ("URLAsset", true),
];

/// `(message type, whether it may be sent asynchronously)`.
const SUPPORTED_MESSAGE_TYPES: &[(&str, bool)] = &[
    ("BBUpdateStatusMsg", false),
    ("CheckpointMsg", true),
    ("CrashLog", true),
    ("DataRequestMsg", false),
    ("FDRSubmit", true),
    ("MsgType", false),
    ("PreviousRestoreLogMsg", false),
    ("ProgressMsg", false),
    ("ProvisioningAck", false),
    ("ProvisioningInfo", false),
    ("ProvisioningStatusMsg", false),
    ("ReceivedFinalStatusMsg", false),
    ("RestoredCrash", true),
    ("StatusMsg", false),
    ("AsyncDataRequestMsg", true),
    ("AsyncWait", true),
    ("RestoreAttestation", true),
];
