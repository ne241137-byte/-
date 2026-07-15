use std::{
    collections::VecDeque,
    env, fs,
    io::{self, BufRead},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

const MAX_FILES_PER_CAMERA: usize = 5;
const CLIP_SECONDS: &str = "5";
const BEFORE_CLIPS: usize = 1;
const AFTER_CLIPS: usize = 1;

const DEFAULT_MEDIA_DIR: &str = "videos";

#[derive(Clone)]
struct CameraConfig {
    role: String,
    label: String,
    input_format: String,
    device: String,
    fps: String,
    size: String,
}

#[derive(Debug)]
struct PendingSave {
    event_id: String,
    label: String,
    target: String,
    clips: Vec<String>,
    after_count: usize,
}

#[derive(Default, Debug)]
struct RecorderState {
    recent_videos: VecDeque<String>,
    pending_saves: Vec<PendingSave>,
}

fn now_stamp() -> io::Result<String> {
    let output = Command::new("date").arg("+%Y%m%d_%H%M%S").output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn env_value(names: &[&str], default: &str) -> String {
    for name in names {
        if let Ok(value) = env::var(name) {
            if !value.trim().is_empty() {
                return value;
            }
        }
    }

    default.to_string()
}

fn default_input_format() -> &'static str {
    if cfg!(target_os = "macos") {
        "avfoundation"
    } else {
        "v4l2"
    }
}

fn default_front_device() -> &'static str {
    if cfg!(target_os = "macos") {
        "1"
    } else {
        "/dev/video0"
    }
}

fn default_rear_device() -> &'static str {
    if cfg!(target_os = "macos") {
        "0"
    } else {
        "/dev/video1"
    }
}

fn camera_config(
    role: &str,
    label: &str,
    default_device: &str,
    device_env: &[&str],
) -> CameraConfig {
    let upper_role = role.to_ascii_uppercase();
    let input_format_name = format!("{upper_role}_INPUT_FORMAT");
    let fps_name = format!("{upper_role}_FPS");
    let size_name = format!("{upper_role}_SIZE");

    CameraConfig {
        role: role.to_string(),
        label: label.to_string(),
        input_format: env_value(
            &[&input_format_name, "DASHCAM_INPUT_FORMAT"],
            default_input_format(),
        ),
        device: env_value(device_env, default_device),
        fps: env_value(&[&fps_name, "DASHCAM_FPS"], "30"),
        size: env_value(&[&size_name, "DASHCAM_SIZE"], "640x480"),
    }
}

fn media_dir() -> String {
    env_value(
        &["DASHCAM_MEDIA_DIR", "VIDEO_VIEWER_DIR"],
        DEFAULT_MEDIA_DIR,
    )
}

fn camera_base_dir(config: &CameraConfig) -> PathBuf {
    Path::new(&media_dir()).join(&config.role)
}

fn continuous_dir(config: &CameraConfig) -> PathBuf {
    camera_base_dir(config).join("continuous")
}

fn target_dir(config: &CameraConfig, target: &str, event_id: &str) -> PathBuf {
    camera_base_dir(config).join(target).join(event_id)
}

fn video_name(config: &CameraConfig, timestamp: &str) -> String {
    format!("{}_video_{timestamp}.mp4", config.role)
}

fn thumb_name_for(video_name: &str) -> String {
    video_name
        .replace("_video_", "_thumb_")
        .strip_suffix(".mp4")
        .map(|base| format!("{base}.jpg"))
        .unwrap_or_else(|| format!("{video_name}.jpg"))
}

fn ensure_dirs(config: &CameraConfig) -> io::Result<()> {
    fs::create_dir_all(continuous_dir(config))?;
    fs::create_dir_all(camera_base_dir(config).join("event"))?;
    fs::create_dir_all(camera_base_dir(config).join("manual"))?;
    Ok(())
}

fn ensure_ffmpeg_exists() -> io::Result<()> {
    let status = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "ffmpegが見つかりません。Macでは brew install ffmpeg、Raspberry Piでは sudo apt install ffmpeg を実行してください。",
        ))
    }
}

fn add_camera_input_args(command: &mut Command, config: &CameraConfig) {
    command.args([
        "-f",
        &config.input_format,
        "-framerate",
        &config.fps,
        "-video_size",
        &config.size,
        "-i",
        &config.device,
    ]);
}

fn initialize_camera(config: &CameraConfig) -> io::Result<()> {
    let init_image = env::temp_dir().join(format!("dashcam_{}_camera_init.jpg", config.role));

    println!("[{}カメラ初期化] カメラを確認しています...", config.label);

    let mut command = Command::new("ffmpeg");
    command.args(["-y", "-hide_banner", "-loglevel", "error"]);
    add_camera_input_args(&mut command, config);
    let status = command.args(["-frames:v", "1"]).arg(&init_image).status()?;

    if init_image.exists() {
        let _ = fs::remove_file(&init_image);
    }

    if status.success() {
        println!("[{}カメラ初期化] 完了しました。", config.label);
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("{}カメラ初期化に失敗しました: {status}", config.label),
        ))
    }
}

fn record_clip(
    config: &CameraConfig,
    video_path: &Path,
    stop_recording: &AtomicBool,
) -> io::Result<()> {
    let mut command = Command::new("ffmpeg");
    command.args(["-y", "-hide_banner", "-loglevel", "error"]);
    add_camera_input_args(&mut command, config);

    let mut child = command
        .args(["-t", CLIP_SECONDS, "-pix_fmt", "yuv420p"])
        .arg(video_path)
        .stdin(Stdio::null())
        .spawn()?;

    loop {
        if stop_recording.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }

        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }

            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("{}録画が失敗しました: {status}", config.label),
            ));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn save_thumbnail(video_path: &Path, thumb_path: &Path) {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-sseof",
            "-0.1",
            "-i",
        ])
        .arg(video_path)
        .args(["-frames:v", "1"])
        .arg(thumb_path)
        .status();

    if let Err(err) = status {
        eprintln!("[サムネイル作成エラー] {err}");
    }
}

fn save_pending_clips(config: &CameraConfig, pending: PendingSave) -> io::Result<()> {
    let output_dir = target_dir(config, &pending.target, &pending.event_id);
    fs::create_dir_all(&output_dir)?;

    let mut saved = 0;
    for clip in pending.clips {
        let src_video = continuous_dir(config).join(&clip);
        let src_thumb = continuous_dir(config).join(thumb_name_for(&clip));
        let dst_video = output_dir.join(format!("{}_{}", pending.target.to_uppercase(), clip));
        let dst_thumb = output_dir.join(format!(
            "{}_{}",
            pending.target.to_uppercase(),
            thumb_name_for(&clip)
        ));

        if src_video.exists() {
            match fs::copy(&src_video, &dst_video) {
                Ok(_) => saved += 1,
                Err(err) => eprintln!(
                    "[{}{}保存エラー] {}: {err}",
                    config.label, pending.label, clip
                ),
            }
        }

        if src_thumb.exists() {
            if let Err(err) = fs::copy(&src_thumb, &dst_thumb) {
                eprintln!(
                    "[{}サムネイル保存エラー] {}: {err}",
                    config.label,
                    src_thumb.display()
                );
            }
        }
    }

    println!(
        "[{}{}保存] {}: {}本の動画を保存しました。",
        config.label, pending.label, pending.event_id, saved
    );
    Ok(())
}

fn cleanup_old_files(config: &CameraConfig, state: &Arc<Mutex<RecorderState>>) -> io::Result<()> {
    let protected_files = {
        let state = state.lock().expect("recorder state mutex poisoned");
        state
            .pending_saves
            .iter()
            .flat_map(|pending| pending.clips.iter().cloned())
            .collect::<Vec<_>>()
    };

    let mut videos = Vec::new();
    for entry in fs::read_dir(continuous_dir(config))? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("mp4") {
            if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
                videos.push(file_name.to_string());
            }
        }
    }

    videos.sort();

    while videos.len() > MAX_FILES_PER_CAMERA {
        let oldest_video = videos.remove(0);

        if protected_files.contains(&oldest_video) {
            continue;
        }

        match fs::remove_file(continuous_dir(config).join(&oldest_video)) {
            Ok(_) => println!(
                "[{}容量確保] 古い録画 {} を削除しました。",
                config.label, oldest_video
            ),
            Err(err) => eprintln!("[{}削除エラー] {}: {err}", config.label, oldest_video),
        }

        let thumb_path = continuous_dir(config).join(thumb_name_for(&oldest_video));
        if let Err(err) = fs::remove_file(&thumb_path) {
            if err.kind() != io::ErrorKind::NotFound {
                eprintln!(
                    "[{}削除エラー] {}: {err}",
                    config.label,
                    thumb_path.display()
                );
            }
        }
    }

    Ok(())
}

fn request_save(states: &[Arc<Mutex<RecorderState>>], prefix: &str, label: &str, target: &str) {
    let stamp = match now_stamp() {
        Ok(stamp) => stamp,
        Err(err) => {
            eprintln!(
                "[{}予約エラー] 現在時刻を取得できませんでした: {err}",
                label
            );
            return;
        }
    };

    let event_id = format!("{prefix}_{stamp}");

    for state in states {
        let mut state = state.lock().expect("recorder state mutex poisoned");
        let pending = PendingSave {
            event_id: event_id.clone(),
            label: label.to_string(),
            target: target.to_string(),
            clips: state.recent_videos.iter().cloned().collect(),
            after_count: AFTER_CLIPS,
        };
        state.pending_saves.push(pending);
    }

    println!(
        "[{}予約] {} フロント/リアの前後1本ずつを保存します。",
        label, event_id
    );
}

fn command_loop(
    states: Vec<Arc<Mutex<RecorderState>>>,
    init_requests: Vec<Arc<AtomicBool>>,
    stop_recording: Arc<AtomicBool>,
) {
    for line in io::stdin().lock().lines() {
        let Ok(command) = line else {
            break;
        };

        match command.trim().to_ascii_uppercase().as_str() {
            "A" | "ACCIDENT" => request_save(&states, "ACCIDENT", "事故", "event"),
            "E" | "EVENT" | "MANUAL" => request_save(&states, "MANUAL", "手動", "manual"),
            "I" | "INIT" => {
                for request in &init_requests {
                    request.store(true, Ordering::SeqCst);
                }
                println!("[カメラ初期化] 次のクリップ開始前にフロント/リアを初期化します。");
            }
            "F" | "STOP" => {
                stop_recording.store(true, Ordering::SeqCst);
                break;
            }
            "" => {}
            other => println!("[コマンド不明] {other}"),
        }
    }

    stop_recording.store(true, Ordering::SeqCst);
}

fn recorder_loop(
    config: CameraConfig,
    state: Arc<Mutex<RecorderState>>,
    init_requested: Arc<AtomicBool>,
    stop_recording: Arc<AtomicBool>,
) -> io::Result<()> {
    ensure_dirs(&config)?;
    println!(
        "[{}設定] input_format={}, device={}, fps={}, size={}",
        config.label, config.input_format, config.device, config.fps, config.size
    );
    initialize_camera(&config)?;

    while !stop_recording.load(Ordering::SeqCst) {
        if init_requested.swap(false, Ordering::SeqCst) {
            initialize_camera(&config)?;
        }

        let timestamp = now_stamp()?;
        let video = video_name(&config, &timestamp);
        let thumb = thumb_name_for(&video);
        let video_path = continuous_dir(&config).join(&video);
        let thumb_path = continuous_dir(&config).join(&thumb);

        println!("[{}録画開始] {}", config.label, video);
        record_clip(&config, &video_path, &stop_recording)?;

        if stop_recording.load(Ordering::SeqCst) {
            break;
        }

        save_thumbnail(&video_path, &thumb_path);
        println!("[{}録画完了] {}", config.label, video);

        let completed = {
            let mut state = state.lock().expect("recorder state mutex poisoned");

            for pending in &mut state.pending_saves {
                pending.clips.push(video.clone());
                if pending.after_count > 0 {
                    pending.after_count -= 1;
                }
            }

            let mut completed = Vec::new();
            let mut index = 0;
            while index < state.pending_saves.len() {
                if state.pending_saves[index].after_count == 0 {
                    completed.push(state.pending_saves.remove(index));
                } else {
                    index += 1;
                }
            }

            state.recent_videos.push_back(video);
            while state.recent_videos.len() > BEFORE_CLIPS {
                state.recent_videos.pop_front();
            }

            completed
        };

        for pending in completed {
            save_pending_clips(&config, pending)?;
        }

        cleanup_old_files(&config, &state)?;
    }

    println!("[{}録画] 終了しました。", config.label);
    Ok(())
}

fn main() -> io::Result<()> {
    ensure_ffmpeg_exists()?;
    fs::create_dir_all(media_dir())?;

    let front_config = camera_config(
        "front",
        "フロント",
        default_front_device(),
        &["FRONT_DEVICE", "DASHCAM_DEVICE"],
    );
    let rear_config = camera_config("rear", "リア", default_rear_device(), &["REAR_DEVICE"]);

    let front_state = Arc::new(Mutex::new(RecorderState::default()));
    let rear_state = Arc::new(Mutex::new(RecorderState::default()));
    let front_init_requested = Arc::new(AtomicBool::new(false));
    let rear_init_requested = Arc::new(AtomicBool::new(false));
    let stop_recording = Arc::new(AtomicBool::new(false));

    let front_thread = {
        let state = Arc::clone(&front_state);
        let init_requested = Arc::clone(&front_init_requested);
        let stop = Arc::clone(&stop_recording);
        thread::spawn(move || recorder_loop(front_config, state, init_requested, stop))
    };

    let rear_thread = {
        let state = Arc::clone(&rear_state);
        let init_requested = Arc::clone(&rear_init_requested);
        let stop = Arc::clone(&stop_recording);
        thread::spawn(move || recorder_loop(rear_config, state, init_requested, stop))
    };

    println!("[ドラレコ] フロント/リア録画プロセスを開始しました。");
    println!("[操作] a=事故保存, e=手動保存, i=カメラ初期化, f=終了");

    command_loop(
        vec![front_state, rear_state],
        vec![front_init_requested, rear_init_requested],
        Arc::clone(&stop_recording),
    );

    for result in [front_thread.join(), rear_thread.join()] {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => eprintln!("[録画エラー] {err}"),
            Err(_) => eprintln!("[録画エラー] 録画スレッドが異常終了しました。"),
        }
    }

    println!("[ドラレコ] 録画を終了しました。");
    Ok(())
}
