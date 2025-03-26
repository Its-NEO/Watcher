use std::{env, error::Error, fs, io::Read, os::unix::fs::MetadataExt, path::{Path, PathBuf}, thread::sleep, time::{Duration, Instant, SystemTime}};
use serde::{Deserialize, Serialize};
use once_cell::sync::Lazy;
use std::time::UNIX_EPOCH;
use chrono::{DateTime, Utc, Local};

static TIMEPERIOD: u32 = 1000000000;

#[derive(Serialize, Deserialize)]
struct Config {
    targets: Vec<String>,
    endpoints: Vec<String>
}

impl Config {
    /// Get the path to the config file 
    fn get_path() -> PathBuf { 
        let cwd = env::current_dir().expect("Error retrieving current working directory");
        cwd.join("watcher.toml")
    }

    /// Generate the default configuration
    fn default() -> Self {
        Config {
            targets: vec![
                "txt".to_string(),
                "json".to_string(),
                "toml".to_string(),
                "rs".to_string(),
            ],
            endpoints: vec![
                "localhost:9996".to_string()
            ]
        }
    }

    /// Save the config to a file 
    fn save(&self) -> Result<(), Box<dyn Error>> {
        let path = Config::get_path();
        let config_str = toml::to_string(self)?;
        fs::write(path, config_str)?;
        Ok(())
    }

    /// Reads the config file and returns its contents as a table
    fn fetch() -> Result<Config, Box<dyn Error>> {
        let path = Config::get_path();
        
        // Try to read the config file, if it doesn't exist, create default
        let config = match fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let config = Config::default();
                config.save()?;
                config
            },
            Err(e) => return Err(Box::new(e)),
        };

        Ok(config)
    }
}

static CONFIG: Lazy<Config> = Lazy::new(|| {
    Config::fetch().unwrap_or(Config::default())
});

#[derive(Debug)]
enum NodeType {
    File, 
    Folder
}

enum FileError {
    Open,
    Metadata,
    TooLarge
}

#[allow(dead_code)]
struct Node {
    kind: NodeType,
    path: PathBuf,
    name: String,
    elapsed: Option<u128>,
    children: Vec<Node>,
    content: Option<String>,
    modified: bool
}

impl Node {
    fn new() -> Self {
        Self {
            kind: NodeType::Folder,
            path: env::current_dir().expect("Failed to read current directory"),
            name: "root".to_string(),
            elapsed: None,
            children: Vec::new(),
            content: None,
            modified: false
        }
    }
    
    fn fill(&mut self, path: &Path) {
        self.path = path.to_path_buf();
        if let Some(name) = path.file_name() {
            self.name = name.to_str().unwrap().to_string();
        }

        self.kind = {
            if path.is_dir() { 
                NodeType::Folder 
            } else {
                NodeType::File
            }
        };

        match self.kind {
            NodeType::File => {
                if path.extension().is_none() ||
                    !CONFIG.targets.contains(&path.extension().unwrap().to_str().unwrap().to_string()) {
                    
                    // Indicator that the file type is invalid
                    self.path = "...".into();
                    return
                }

                self.elapsed = match path.metadata() {
                    Ok(t) => {
                        Some(t.modified().expect("Error retrieving metadata").duration_since(UNIX_EPOCH).unwrap().as_millis())
                    },
                    _ => {
                        None
                    }
                };
                
                self.content = match self.read() {
                    Ok(t) => Some(t),
                    Err(_) => None
                }
            },
            NodeType::Folder => {
                self.elapsed = None;

                for res in match path.read_dir() {
                    Ok(t) => t,
                    _ => return
                } {
                    let entry: fs::DirEntry = res.expect("Invalid Entry");
                    let mut child: Node = Node::new();
                    child.fill(&entry.path());

                    if child.path.to_str().unwrap() == "..." {
                        continue;
                    }

                    if matches!(child.kind, NodeType::Folder) && child.children.is_empty() {
                        continue;
                    }
                    
                    self.children.push(child);
                }
            }
        };
    }

    fn display(&self, prev: &str) {
        let mut name_column = format!("{}└── {}", prev, self.name);
        
        const MAX_WIDTH: usize = 60;
        const OFFSET: usize = 20;

        if self.name.len() > MAX_WIDTH {
            let mut name = self.name.clone();
            name.truncate(MAX_WIDTH);
            name_column = format!("{}└── {}...", prev, name);
        }

        println!("{:.<width$} Last Modified: -{} millis", name_column, self.elapsed.unwrap_or(u128::MAX), width=MAX_WIDTH + OFFSET);

        for child in &self.children {
            child.display(&format!("{}│  ", prev));
        }
    }

    #[allow(dead_code)]
    fn read(&self) -> core::result::Result<String, FileError> {
        let mut file: fs::File = match fs::File::open(self.path.clone()) {
            Ok(t) => t,
            _ => return Err(FileError::Open)
        };

        let metadata: fs::Metadata = match file.metadata() {
            Ok(t) => t,
            _ => return Err(FileError::Metadata)
        };

        if metadata.size() > 1024 * 1024 * 10 {
            return Err(FileError::TooLarge)
        }

        let mut buffer: String = String::new();
        let _ = file.read_to_string(&mut buffer);

        Ok(buffer)
    }

    fn poll(&mut self, buffer: &mut Vec<Notification>) {
        let elapsed: Option<u128> = match self.path.metadata() {
            Ok(t) => {
                Some(t.modified().expect("Error retrieving metadata").duration_since(UNIX_EPOCH).unwrap().as_millis())
            },
            _ => {
                None
            }
        };

        if matches!(self.kind, NodeType::File) && self.elapsed != elapsed {
            // change noticed
            let mut notifs = Notification::new(&self.path);
            let option_new_lines = match self.read() {
                Ok(t) => Some(t),
                _ => None
            };

            let option_old_lines = self.content.clone();
            self.content = option_new_lines.clone();

            if option_new_lines.is_some() && option_old_lines.is_some() {
                let old_lines = option_old_lines.unwrap();
                let new_lines = option_new_lines.unwrap();

                let mut diff_output = Vec::new();
                diff::lines(&old_lines, &new_lines).iter().for_each(|change| {
                    match change {
                        diff::Result::Left(l) => diff_output.push(diff::Result::Left(l.to_string())),
                        diff::Result::Both(l, r) => diff_output.push(diff::Result::Both(l.to_string(), r.to_string())),
                        diff::Result::Right(r) => diff_output.push(diff::Result::Right(r.to_string())),
                    }
                });

                notifs.diff = diff_output;
                buffer.push(notifs);
            }
        }

        self.elapsed = elapsed;
        
        for child in &mut self.children {
            child.poll(buffer);
        }
    }
}

struct Notification {
    time: SystemTime, 
    path: PathBuf,
    diff: Vec<diff::Result<String>>
}

impl Notification {
    fn new(path: &Path) -> Self {
        Self {
            time: SystemTime::now(),
            path: path.to_path_buf().clone(),
            diff: Vec::new()
        }
    }

    fn format_system_time(time: &SystemTime) -> String {
        let datetime = match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => {
                let secs = duration.as_secs() as i64;
                let nanos = duration.subsec_nanos();
                DateTime::<Utc>::from_timestamp(secs, nanos).unwrap()
            },
            Err(_) => {
                Utc::now() 
            }
        };

        let local_time: DateTime<Local> = DateTime::from(datetime);
        local_time.format("%Y-%m-%d %H:%M:%S").to_string()
    }

    fn display(&self) {
        println!("[{}] - {}", Notification::format_system_time(&self.time), self.path.as_os_str().to_str().unwrap());
        let mut count: u64 = 0;
        let _ = &self.diff.iter().for_each(|diff| {
            count += 1;
            match diff {
                diff::Result::Left(l) => {
                    println!("{:0>5} - |  {}", count, l);
                },
                diff::Result::Right(r) => {
                    println!("{:0>5} + |  {}", count, r);
                }
                _ => {},
            }
        });
    }

    fn json(&self) -> String {
        let datetime: DateTime<Utc> = self.time.into();
        let rfc_dt = datetime.to_rfc3339();

        #[derive(serde::Serialize, serde::Deserialize)]
        struct Change {
            direction: i8,
            change: String
        }

        let mut diff_result: Vec<Change> = Vec::new();

        self.diff.iter().for_each(|change| {
            match change {
                diff::Result::Left(l) => diff_result.push(Change{direction: -1, change: l.to_string()}),
                diff::Result::Right(r) => diff_result.push(Change{direction: 1, change: r.to_string()}),
                diff::Result::Both(l, _) => diff_result.push(Change{direction: 0, change: l.to_string()}),
            }
        });

        let json = serde_json::json! ({
            "time": rfc_dt,
            "path": self.path.to_str(),
            "diff": diff_result
        });

        serde_json::to_string(&json).unwrap()
    }

    async fn notify(&self) -> Result<(), reqwest::Error> {
        let client = reqwest::Client::new();
        
        for endpoint in &CONFIG.endpoints {
            client.post(endpoint)
                .body(self.json())
                .send()
                .await?;
        }
        
        Ok(())
    }
}

struct FileTree {
    head: Box<Node>,
}

impl FileTree {
    fn new() -> FileTree {
        FileTree { head: Box::new(Node::new()) }
    }

    fn fill(&mut self) {
        let cwd = env::current_dir().expect("Current directory retrieval failed");
        self.head.fill(cwd.as_path());
    }

    #[allow(dead_code)]
    fn display(&self) {
        self.head.display("");
    }
}

#[tokio::main]
async fn main() {
    let mut ft = FileTree::new();
    ft.fill();

    let mut notifications: Vec<Notification> = Vec::new();
    let mut cycle = 0;
    const TREE_REBUILD_CYCLE: usize = 1000;

    loop {
        cycle += 1;

        ft.head.poll(&mut notifications);
        if let Some(notif) = notifications.pop() {
            notif.display();
            let _ = notif.notify().await;
        }

        if cycle == TREE_REBUILD_CYCLE {
            ft = FileTree::new();
            ft.fill();

            cycle = 0;
        }

        sleep(Duration::new(0, TIMEPERIOD));
        
    }
}

/* TODO: Potential bug:
 *
 * If the tree is taking a long time to construct, a tree can take more time to
 * construct itself than the time period mentioned.
 *
 * If that is the case, even if a modification is done to a file, for the next iteration,
 * the tree would take more time than the threshold for a file modification. The 
 * last_modification time for the file would have been expired by then and the file
 * could be marked as an unmodified file.
 *
 * Potential solutions:
 * - Add the time to construct the tree as an offset to the original timeperiod ❌
 * OR
 * - Compare the last modification time for both the versions, if they are different, it is
 * modified ✅
 *
 * The latter seems like the better approach.
 * 
 */

/* TODO: Inner data model fix
 *
 * Instead of relying on two snapshots that we are creating, we can create one snapshot of the
 * filesystem and whenever it needs to update itself, we create a diff when we compare the tree
 * with its new version. This saves space and time.
 *
 */

