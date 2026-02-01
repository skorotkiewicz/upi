use anyhow::{Context, Result};
use std::sync::Arc;

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{self, Duration};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to the configuration file (YAML)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Global check interval in seconds
    #[arg(long, rename_all = "kebab-case")]
    global_check_every: Option<u64>,

    /// State file path
    #[arg(short, long, default_value = "upi-state.json")]
    state_file: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Task {
    url: String,
    parse: String,
    command: String,
    #[serde(rename = "check-every")]
    check_every: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AppConfig {
    #[serde(default, rename = "global-check-every")]
    global_check_every: Option<u64>,
    #[serde(default)]
    tasks: Vec<Task>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct State {
    // Map URL to the last parsed output
    results: HashMap<String, String>,
}

impl State {
    fn load(path: &Path) -> Self {
        if let Ok(content) = std::fs::read_to_string(path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

async fn run_task(task: &Task, state: &mut State, client: &reqwest::Client) -> Result<bool> {
    println!("Checking URL: {}", task.url);
    
    // 1. Download
    let resp = client.get(&task.url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Failed to fetch URL {}: {}", task.url, resp.status());
    }
    let response = resp.text().await?;
    
    // 2. Parse (using the provided command via shell)
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&task.parse)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn parse command")?;

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    stdin.write_all(response.as_bytes()).await?;
    drop(stdin);

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Parse command failed: {}", err);
    }
    
    let parsed_text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // println!("Parsed text: '{}'", parsed_text);
    
    // 3. Compare with state
    let last_result = state.results.get(&task.url);
    let changed = match last_result {
        Some(last) => last != &parsed_text,
        None => true,
    };

    if changed {
        println!("Change detected for {}. Running command: {}", task.url, task.command);
        state.results.insert(task.url.clone(), parsed_text.clone());
        
        // 4. Run command
        let cmd_status = Command::new("sh")
            .arg("-c")
            .arg(&task.command)
            .env("UPI_PARSED", &parsed_text)
            .status()
            .await?;
            
        if !cmd_status.success() {
            println!("Warning: Command for {} exited with error", task.url);
        }
        return Ok(true);
    } else {
        println!("No change for {}", task.url);
    }

    Ok(false)
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    
    let mut config = if let Some(config_path) = cli.config {
        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {:?}", config_path))?;
        serde_yaml::from_str::<AppConfig>(&content)
            .with_context(|| "Failed to parse YAML config")?
    } else {
        AppConfig {
            global_check_every: cli.global_check_every,
            tasks: vec![],
        }
    };

    // If CLI provided a global check interval, it overrides config
    if cli.global_check_every.is_some() {
        config.global_check_every = cli.global_check_every;
    }

    let state_file = cli.state_file.clone();
    
    let client = reqwest::Client::builder()
        .user_agent("upi/0.1.0")
        .build()?;

    if config.tasks.is_empty() {
        println!("No tasks defined in config. Exiting.");
        return Ok(());
    }

    println!("Starting upi with {} tasks", config.tasks.len());

    // We'll spawn a task for each task interval, and optionally a global one.
    // However, to keep it simple and avoid concurrent state writes, we can use a single loop 
    // or a shared state with a mutex.
    
    use tokio::sync::Mutex;
    let state = Arc::new(Mutex::new(State::load(&state_file)));
    
    let mut set = tokio::task::JoinSet::new();

    // Spawn individual tasks
    for task in config.tasks.clone() {
        let state = Arc::clone(&state);
        let state_file = state_file.clone();
        let client = client.clone();
        set.spawn(async move {
            let mut interval = time::interval(Duration::from_secs(task.check_every));
            loop {
                interval.tick().await;
                let mut s = state.lock().await;
                match run_task(&task, &mut s, &client).await {
                    Ok(changed) => {
                        if changed {
                            if let Err(e) = s.save(&state_file) {
                                println!("Error saving state: {}", e);
                            }
                        }
                    }
                    Err(e) => println!("Error running task {}: {}", task.url, e),
                }
            }
        });
    }

    // Spawn global task if enabled
    if let Some(global_secs) = config.global_check_every {
        if global_secs > 0 {
            let state = Arc::clone(&state);
            let state_file = state_file.clone();
            let tasks = config.tasks.clone();
            let client = client.clone();
            set.spawn(async move {
                let mut interval = time::interval(Duration::from_secs(global_secs));
                loop {
                    interval.tick().await;
                    println!("Global check triggered...");
                    let mut s = state.lock().await;
                    let mut any_changed = false;
                    for task in &tasks {
                        match run_task(task, &mut s, &client).await {
                            Ok(changed) => if changed { any_changed = true; },
                            Err(e) => println!("Error running task {} (global): {}", task.url, e),
                        }
                    }
                    if any_changed {
                        if let Err(e) = s.save(&state_file) {
                            println!("Error saving state: {}", e);
                        }
                    }
                }
            });
        }
    }

    // Wait for all tasks (they run forever)
    while let Some(res) = set.join_next().await {
        res?;
    }

    Ok(())
}
