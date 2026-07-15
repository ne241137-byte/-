# Dashcam Rust Viewer Layout

ローカルWebビューアに合わせたドラレコ録画システムです。

今まで作った `dashcam_rust_no_crates` は残したまま、こちらは別版として作っています。

## 目的

Webビューアのデフォルト動画フォルダ `videos/` に、フロント/リアの録画をカテゴリ別に保存します。

```text
videos/
  front/
    continuous/  フロント常時録画
    event/       フロント事故イベント録画
    manual/      フロント手動録画
  rear/
    continuous/  リア常時録画
    event/       リア事故イベント録画
    manual/      リア手動録画
```

## セットアップ

Mac:

```sh
brew install rust ffmpeg
```

Raspberry Pi 5:

```sh
sudo apt update
sudo apt install -y ffmpeg rustc cargo v4l-utils
```

## カメラ確認

Mac:

```sh
ffmpeg -f avfoundation -list_devices true -i ""
```

Raspberry Pi 5:

```sh
v4l2-ctl --list-devices
ls /dev/video*
```

## 実行

Macで試す例:

```sh
cd /Users/kojiyoshi/Documents/Codex/2026-07-01/ma/outputs/dashcam_rust_viewer_layout
FRONT_DEVICE=1 REAR_DEVICE=0 cargo run --offline
```

Raspberry Pi 5でUSBカメラ2台を使う例:

```sh
cd /path/to/dashcam_rust_viewer_layout
FRONT_DEVICE=/dev/video0 REAR_DEVICE=/dev/video1 DASHCAM_FPS=30 DASHCAM_SIZE=1280x720 cargo run --offline
```

## 操作

- `a`: 事故イベントとして保存
- `e`: 手動イベントとして保存
- `i`: フロント/リアのカメラ初期化
- `f`: 録画停止

## Webビューアとの接続

Webビューア側の動画フォルダを、このプロジェクトの `videos/` に向けます。

例:

```json
{
  "video_dir": "videos"
}
```

ビューアを別フォルダで動かす場合は、録画側の保存先を絶対パスで指定できます。

```sh
DASHCAM_MEDIA_DIR=/home/pi/videos FRONT_DEVICE=/dev/video0 REAR_DEVICE=/dev/video1 cargo run --offline
```

この場合、Webビューア側も `/home/pi/videos` を参照してください。
