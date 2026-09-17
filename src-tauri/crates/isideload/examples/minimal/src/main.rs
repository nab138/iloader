use idevice::usbmuxd::{UsbmuxdAddr, UsbmuxdConnection};
use isideload::{
    anisette::remote_v3::RemoteV3AnisetteProvider,
    auth::apple_account::{AppleAccount, TwoFactorCallbackParams, TwoFactorCallbackResponse},
    dev::{
        certificates::DevelopmentCertificate, developer_session::DeveloperSession,
        teams::DeveloperTeam,
    },
    sideload::{SideloaderBuilder, TeamSelection, builder::MaxCertsBehavior},
    util::keyring_storage::KeyringStorage,
};
use std::{env, path::PathBuf};

use tracing::Level;
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    isideload::init().expect("Failed to initialize error reporting");
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let args: Vec<String> = env::args().collect();

    let apple_id = args
        .get(1)
        .expect("Please provide the Apple ID to use for installation");
    let apple_password = args.get(2).expect("Please provide the Apple ID password");
    let app_path = PathBuf::from(
        args.get(3).unwrap_or(&"".to_string()), // .expect("Please provide the path to the app to install"),
    );

    let get_2fa_code = async |params: TwoFactorCallbackParams| {
        let mut code = String::new();

        if params.unknown {
            println!(
                "The most recently attempted 2FA Method failed, please try a different method."
            );
        } else {
            println!(
                "Enter the 2FA code sent to {}:",
                if params.sms {
                    params
                        .numbers
                        .iter()
                        .find(|n| n.id == params.selected_number_id.unwrap())
                        .unwrap()
                        .number_with_dial_code
                        .clone()
                } else {
                    "your devices".to_string()
                }
            );
        }

        let other_numbers: Vec<_> = params
            .numbers
            .iter()
            .filter(|n| Some(n.id) != params.selected_number_id)
            .collect();

        if params.unknown {
            println!("Enter \"d\" to have the code sent to your devices.");
        }
        if params.sms && !params.unknown {
            println!("Or, enter \"d\" to have the code sent to your devices instead.");
        }

        if !other_numbers.is_empty() {
            println!(
                "Or, select one of these numbers to receive the code instead. (Type \"p<id>\" to select, e.g. \"p1\"):"
            );
            for (_, n) in other_numbers.iter().enumerate() {
                println!("ID {}: {}", n.id, n.number_with_dial_code);
            }
        }

        if !params.unknown {
            println!("Enter \"r\" to resend the code.");
        }

        std::io::stdin().read_line(&mut code).unwrap();

        if code.trim().starts_with('p') {
            let selected_id = code.trim()[1..].parse::<u32>().unwrap();
            return Ok(TwoFactorCallbackResponse::SendSms(selected_id));
        }

        if code.trim() == "d" {
            return Ok(TwoFactorCallbackResponse::SendToDevices);
        }

        if code.trim() == "r" && !params.unknown {
            return Ok(TwoFactorCallbackResponse::ResendCode);
        }

        Ok(TwoFactorCallbackResponse::SubmitCode(
            code.trim().to_string(),
        ))
    };

    let account = AppleAccount::builder(apple_id)
        .anisette_provider(
            RemoteV3AnisetteProvider::default()
                .unwrap()
                .set_serial_number("2".to_string()),
        )
        .login(apple_password, get_2fa_code)
        .await;

    let mut account = account.unwrap();

    let dev_session = DeveloperSession::from_account(&mut account)
        .await
        .expect("Failed to create developer session");

    let usbmuxd = UsbmuxdConnection::default().await;
    if usbmuxd.is_err() {
        panic!("Failed to connect to usbmuxd: {:?}", usbmuxd.err());
    }
    let mut usbmuxd = usbmuxd.unwrap();

    let devs = usbmuxd.get_devices().await.unwrap();
    if devs.is_empty() {
        panic!("No devices found");
    }

    let provider = devs
        .first()
        .unwrap()
        .to_provider(UsbmuxdAddr::from_env_var().unwrap(), "isideload-demo");

    let team_selection_prompt = |teams: &Vec<DeveloperTeam>| {
        println!("Please select a team:");
        for (index, team) in teams.iter().enumerate() {
            println!(
                "{}: {} ({})",
                index + 1,
                team.name.as_deref().unwrap_or("<Unnamed>"),
                team.team_id
            );
        }
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        let selection = input.trim().parse::<usize>().ok()?;
        if selection == 0 || selection > teams.len() {
            return None;
        }
        Some(teams[selection - 1].team_id.clone())
    };

    let cert_selection_prompt = |certs: &Vec<DevelopmentCertificate>| {
        println!("Maximum number of certificates reached. Please select certificates to revoke:");
        for (index, cert) in certs.iter().enumerate() {
            println!(
                "({}) {}: {}",
                index + 1,
                cert.name.as_deref().unwrap_or("<Unnamed>"),
                cert.machine_name.as_deref().unwrap_or("<No Machine Name>"),
            );
        }
        println!("Enter the numbers of the certificates to revoke, separated by commas:");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        let selections: Vec<usize> = input
            .trim()
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
            .filter(|&n| n > 0 && n <= certs.len())
            .collect();
        if selections.is_empty() {
            return None;
        }
        Some(
            selections
                .into_iter()
                .map(|n| certs[n - 1].serial_number.clone().unwrap_or_default())
                .collect::<Vec<_>>(),
        )
    };

    let mut sideloader = SideloaderBuilder::new(dev_session, apple_id.to_string())
        .team_selection(TeamSelection::PromptOnce(team_selection_prompt))
        .max_certs_behavior(MaxCertsBehavior::Prompt(Box::new(cert_selection_prompt)))
        .storage(Box::new(KeyringStorage::new("minimal".to_string())))
        .machine_name("isideload-minimal".to_string())
        .build();

    let result = sideloader
        .install_app(
            &provider,
            app_path,
            true,
            None::<fn(f32) -> std::future::Ready<()>>,
        )
        .await;
    match result {
        Ok(_) => println!("App installed successfully"),
        Err(e) => panic!("{}", e),
    }
}
