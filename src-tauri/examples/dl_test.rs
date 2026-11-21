// Отладка: добавляет magnet и печатает статистику раз в 2 секунды.
use librqbit::{AddTorrent, AddTorrentOptions, Session};
use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let magnet = std::env::args().nth(1).expect("magnet");
    let out = std::env::temp_dir().join("linfix-dltest");
    let session = Session::new(out.clone()).await?;
    let t = Instant::now();
    println!("adding… (ждём метаданные)");
    let h = session
        .add_torrent(
            AddTorrent::from_url(magnet),
            Some(AddTorrentOptions {
                overwrite: true,
                list_only: false,
                output_folder: Some(out.to_string_lossy().into()),
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .unwrap();
    println!("metadata ok за {:?}: {:?}", t.elapsed(), h.name());
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let s = h.stats();
        println!(
            "{} {}/{} {:?}",
            s.state,
            s.progress_bytes,
            s.total_bytes,
            s.live.map(|l| l.download_speed.mbps)
        );
    }
    Ok(())
}
