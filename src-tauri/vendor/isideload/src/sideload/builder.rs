use std::fmt::Display;

use crate::{
    dev::{
        certificates::DevelopmentCertificate, developer_session::DeveloperSession,
        teams::DeveloperTeam,
    },
    sideload::sideloader::Sideloader,
    util::storage::SideloadingStorage,
};

/// Configuration for selecting a developer team during sideloading
///
/// If there is only one team, it will be selected automatically regardless of this setting.
/// If there are multiple teams, the behavior will depend on this setting.
#[derive(Clone)]
pub enum TeamSelection {
    /// Select the first team automatically
    First,
    /// Prompt the user to select a team the first time this sideloader is used, and remember the selection for future runs
    PromptOnce(fn(&Vec<DeveloperTeam>) -> Option<String>),
    /// Prompt the user to select a team every time this sideloader is used
    PromptAlways(fn(&Vec<DeveloperTeam>) -> Option<String>),
}

impl Display for TeamSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamSelection::First => write!(f, "first team"),
            TeamSelection::PromptOnce(_) => write!(f, "prompting for team (once)"),
            TeamSelection::PromptAlways(_) => write!(f, "prompting for team (always)"),
        }
    }
}

/// Behavior when the maximum number of development certificates is reached
pub enum MaxCertsBehavior {
    /// If the maximum number of certificates is reached, revoke certs until it is possible to create a new certificate
    Revoke,
    /// If the maximum number of certificates is reached, return an error instead of creating a new certificate
    Error,
    /// If the maximum number of certificates is reached, prompt the user to select which certificates to revoke until it is possible to create a new certificate
    Prompt(Box<dyn Fn(&Vec<DevelopmentCertificate>) -> Option<Vec<String>> + Send + Sync>),
}

/// The actual behavior choices for extensions (non-prompt variants)
pub enum ExtensionsBehaviorChoice {
    /// Use the main app id/profile for all sub-bundles
    ReuseMain,
    /// Create separate app ids/profiles for each sub-bundle
    RegisterAll,
    /// Remove all sub-bundles
    RemoveExtensions,
}

// /// Behavior used when an app contains sub bundles
// pub enum ExtensionsBehavior {
//     /// Use the main app id/profile for all sub-bundles
//     ReuseMain,
//     /// Create separate app ids/profiles for each sub-bundle
//     RegisterAll,
//     /// Remove all sub-bundles
//     RemoveExtensions,
//     /// Prompt the user to choose one of the above behaviors
//     Prompt(fn(&Vec<String>) -> ExtensionsBehaviorChoice),
// }

// impl From<ExtensionsBehaviorChoice> for ExtensionsBehavior {
//     fn from(choice: ExtensionsBehaviorChoice) -> Self {
//         match choice {
//             ExtensionsBehaviorChoice::ReuseMain => ExtensionsBehavior::ReuseMain,
//             ExtensionsBehaviorChoice::RegisterAll => ExtensionsBehavior::RegisterAll,
//             ExtensionsBehaviorChoice::RemoveExtensions => ExtensionsBehavior::RemoveExtensions,
//         }
//     }
// }

pub struct SideloaderBuilder {
    developer_session: DeveloperSession,
    apple_email: String,
    team_selection: Option<TeamSelection>,
    max_certs_behavior: Option<MaxCertsBehavior>,
    //extensions_behavior: Option<ExtensionsBehavior>,
    storage: Option<Box<dyn SideloadingStorage>>,
    machine_name: Option<String>,
    delete_app_after_install: bool,
}

impl SideloaderBuilder {
    /// Create a new `SideloaderBuilder` with the provided Apple developer session and Apple ID email.
    pub fn new(developer_session: DeveloperSession, apple_email: String) -> Self {
        SideloaderBuilder {
            team_selection: None,
            storage: None,
            developer_session,
            machine_name: None,
            apple_email,
            max_certs_behavior: None,
            delete_app_after_install: true,
            // extensions_behavior: None,
        }
    }

    /// Set the team selection behavior
    ///
    /// See [`TeamSelection`] for details.
    pub fn team_selection(mut self, selection: TeamSelection) -> Self {
        self.team_selection = Some(selection);
        self
    }

    /// Set the storage backend for sideloading data
    ///
    /// An implementation using `keyring` is provided in the `keyring-storage` feature.
    /// See [`SideloadingStorage`] for details.
    ///
    /// If not set, either keyring storage or in memory storage (not persisted across runs) will be used depending on if the `keyring-storage` feature is enabled.
    pub fn storage(mut self, storage: Box<dyn SideloadingStorage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Set the machine name to use for the development certificate
    ///
    /// This has no bearing on functionality but can be useful for users to identify where a certificate came from.
    /// If not set, a default name of "isideload" will be used.
    pub fn machine_name(mut self, machine_name: String) -> Self {
        self.machine_name = Some(machine_name);
        self
    }

    /// Set the behavior for when the maximum number of development certificates is reached
    pub fn max_certs_behavior(mut self, behavior: MaxCertsBehavior) -> Self {
        self.max_certs_behavior = Some(behavior);
        self
    }

    /// Set whether to delete the signed app from the temporary storage after installation. Defaults to `true`.
    pub fn delete_app_after_install(mut self, delete: bool) -> Self {
        self.delete_app_after_install = delete;
        self
    }

    // pub fn extensions_behavior(mut self, behavior: ExtensionsBehavior) -> Self {
    //     self.extensions_behavior = Some(behavior);
    //     self
    // }

    /// Build the `Sideloader` instance with the provided configuration
    pub fn build(self) -> Sideloader {
        Sideloader::new(
            self.developer_session,
            self.apple_email,
            self.team_selection.unwrap_or(TeamSelection::First),
            self.max_certs_behavior.unwrap_or(MaxCertsBehavior::Error),
            self.machine_name.unwrap_or_else(|| "isideload".to_string()),
            self.storage
                .unwrap_or_else(|| Box::new(crate::util::storage::new_storage())),
            // self.extensions_behavior
            //     .unwrap_or(ExtensionsBehavior::RegisterAll),
            self.delete_app_after_install,
        )
    }
}
