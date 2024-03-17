#[macro_use]
extern crate log;

use std::fs::File;
use std::io::Write;
use std::{fs, thread};

use axum::{
    extract::State,
    handler::HandlerWithoutStateExt,
    http::{Request, StatusCode},
    middleware::map_request_with_state,
    Router,
};
use std::net::SocketAddr;
use tower_http::{services::ServeDir, trace::TraceLayer};

use clap::Parser;

use pnet::datalink;
use tokio::sync::Mutex;
use tokio::time;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    #[arg(short, long, default_value_t = 8766)]
    port: u16,
    #[arg(short, long)]
    target_directory: String,
    #[arg(short, long, default_value_t = 60000)]
    exit_timer: u32,
}

#[derive(Clone)]
struct AppState {}

static mut FILE_LIST: Mutex<Vec<String>> = Mutex::const_new(Vec::new());

async fn my_middleware<B>(State(_state): State<AppState>, request: Request<B>) -> Request<B> {
    let mut locked = unsafe { FILE_LIST.lock().await };

    debug!("Current list: {:?}", locked);

    let mut name = request.uri().to_string().replace("%20", " "); // URL space
    name.remove(0);

    info!("Requested: {:?}", name);

    let index = locked.iter().position(|r| r == &name);
    if index.is_some() {
        let index_some = index.unwrap();
        locked.remove(index_some);
    } else {
        error!("Requested a file that was not expected");
    }

    request
}

#[tokio::main]
async fn main() {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );

    let args = Args::parse();

    let mut interfaces = String::new();
    for iface in datalink::interfaces() {
        for ip in iface.ips {
            if ip.is_ipv4() {
                interfaces.push_str(&format!("{}:{} ", ip.ip(), args.port));
            }
        }
    }
    info!("Starting, available at {}", interfaces);

    let mut file_names: Vec<String> = Vec::new();

    match fs::read_dir(args.target_directory.clone()) {
        Ok(entries) => {
            file_names = entries
                .filter_map(|entry| entry.ok().and_then(|e| e.file_name().into_string().ok()))
                .collect();
        }
        Err(e) => {
            eprintln!("Error: {}", e);
        }
    }

    {
        let mut locked = unsafe { FILE_LIST.lock().await };
        *locked = file_names.clone();
    }

    let mut file =
        File::create(format!("{}/{}", args.target_directory.clone(), "index.txt")).unwrap();

    file.write_all(file_names.join("\n").as_bytes()).unwrap();

    info!("Starting download server");

    let target_dir_clone = args.target_directory.clone();
    let server_thread = tokio::spawn(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], args.port));

        async fn handle_404() -> (StatusCode, &'static str) {
            (StatusCode::NOT_FOUND, "Not found")
        }

        let server_dir =
            ServeDir::new(&target_dir_clone).not_found_service(handle_404.into_service());

        let state = AppState { /* ... */ };

        let app = Router::new()
            .nest_service("", server_dir)
            .route_layer(map_request_with_state(state.clone(), my_middleware));

        axum::Server::bind(&addr)
            .serve(app.layer(TraceLayer::new_for_http()).into_make_service())
            .await
            .unwrap();
    });

    let loop_thread = tokio::spawn(async move {
        loop {
            thread::sleep(time::Duration::from_millis(5000));

            let locked = unsafe { FILE_LIST.lock().await };
            if locked.is_empty() {
                thread::sleep(time::Duration::from_millis(args.exit_timer.into()));
                info!("Downloaded all files");
                break;
            }
        }
    });

    loop_thread.await.unwrap();
    server_thread.abort();
    info!("Finished downloading all files, exiting...");
}
