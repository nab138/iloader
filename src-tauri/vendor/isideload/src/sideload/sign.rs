use std::collections::BTreeMap;

use apple_codesign::{
    BundleSigningSettings, ProvisioningProfile, RustCryptoCmsSigner, sign_bundle,
};

use plist::Dictionary;
use rootcause::prelude::*;

use crate::{
    dev::{app_ids::Profile, teams::DeveloperTeam},
    sideload::{
        application::{Application, SpecialApp},
        cert_identity::CertificateIdentity,
    },
};

pub async fn sign<F, Fut>(
    app: &mut Application,
    cert_identity: &CertificateIdentity,
    main_provisioning_profile: &Profile,
    all_profiles: &Vec<(String, Profile, Dictionary)>,
    special: &Option<SpecialApp>,
    team: &DeveloperTeam,
    progress_callback: Option<F>,
) -> Result<(), Report>
where
    F: Fn(f32) -> Fut,
    Fut: Future<Output = ()>,
{
    let profile = ProvisioningProfile::parse(main_provisioning_profile.encoded_profile.as_ref())?;
    let certificate_chain = cert_identity.profile_to_certificate_chain(&profile)?;

    let signer = RustCryptoCmsSigner::new(
        cert_identity.private_key.clone(),
        cert_identity.certificate.clone(),
        certificate_chain,
    );
    if let Some(callback) = &progress_callback {
        callback(0.4).await;
    }

    let mut entitlements = profile.entitlements().clone();
    if matches!(
        special,
        Some(SpecialApp::SideStoreLc) | Some(SpecialApp::LiveContainer)
    ) {
        let mut keychain_access = vec![plist::Value::String(format!(
            "{}.com.kdt.livecontainer.shared",
            team.team_id
        ))];

        for number in 1..128 {
            keychain_access.push(plist::Value::String(format!(
                "{}.com.kdt.livecontainer.shared.{}",
                team.team_id, number
            )));
        }

        entitlements.insert(
            "keychain-access-groups".to_string(),
            plist::Value::Array(keychain_access),
        );
    }

    let mut settings = BundleSigningSettings::new(&team.team_id, entitlements, Some(&signer));
    settings.embedded_mobileprovision = Some(main_provisioning_profile.encoded_profile.as_ref());

    settings.embedded_mobileprovisions_by_bundle_id = all_profiles
        .iter()
        .map(|(bundle_id, data, _)| (bundle_id.clone(), data.encoded_profile.as_ref()))
        .collect::<BTreeMap<_, _>>();
    settings.entitlements_by_bundle_id = all_profiles
        .iter()
        .map(|(bundle_id, _, entitlements)| (bundle_id.clone(), entitlements.clone()))
        .collect();

    if let Some(callback) = &progress_callback {
        callback(0.5).await;
    }

    Ok(sign_bundle(&app.bundle.bundle_dir, &settings)?)
}
