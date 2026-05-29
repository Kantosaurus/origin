// SPDX-License-Identifier: Apache-2.0
//! Throughput benchmark — sequential write + read of 64 MiB through the uring
//! path. We do not depend on criterion to keep the bench harness light.

#[cfg(all(target_os = "linux", feature = "uring"))]
mod linux_bench {
    use origin_cas::{packfile_uring::read_at_uring, Hash, PackBuilder, PackReader};
    use std::time::Instant;
    use tempfile::TempDir;

    fn write_bench() -> f64 {
        let dir = TempDir::new().expect("tmpdir");
        let path = dir.path().join("bench.pack");
        let mut b = PackBuilder::create(&path).expect("create");
        let chunk = vec![0xABu8; 64 * 1024];
        let total = 64 * 1024 * 1024;
        let count = total / chunk.len();
        let start = Instant::now();
        for i in 0..count {
            let h = Hash::of(&[i as u8; 32]); // distinct synthetic hash
            b.append(h, &chunk).expect("append");
        }
        b.finalize().expect("finalize");
        let elapsed = start.elapsed().as_secs_f64();
        (total as f64) / (1024.0 * 1024.0) / elapsed
    }

    fn read_bench() -> f64 {
        let dir = TempDir::new().expect("tmpdir");
        let path = dir.path().join("bench.pack");
        let mut b = PackBuilder::create(&path).expect("create");
        let chunk = vec![0xCDu8; 64 * 1024];
        let total = 64 * 1024 * 1024;
        let count = total / chunk.len();
        let mut hashes = Vec::with_capacity(count);
        for i in 0..count {
            let h = Hash::of(&[i as u8; 32]);
            b.append(h, &chunk).expect("append");
            hashes.push(h);
        }
        b.finalize().expect("finalize");
        let reader = PackReader::open(&path).expect("open");
        let throughput = std::sync::Mutex::new(0.0_f64);
        tokio_uring::start(async {
            let start = Instant::now();
            for h in hashes {
                let _ = read_at_uring(&reader, h).await.expect("read");
            }
            let elapsed = start.elapsed().as_secs_f64();
            *throughput.lock().expect("throughput lock") = (total as f64) / (1024.0 * 1024.0) / elapsed;
        });
        let g = *throughput.lock().expect("throughput lock");
        g
    }

    pub fn run() {
        let w = write_bench();
        let r = read_bench();
        eprintln!("uring write MiB/s = {w:.1}");
        eprintln!("uring read  MiB/s = {r:.1}");
        assert!(w >= 180.0, "write threshold not met: {w:.1} MiB/s");
        assert!(r >= 250.0, "read threshold not met: {r:.1} MiB/s");
    }
}

#[cfg(all(target_os = "linux", feature = "uring"))]
fn main() {
    linux_bench::run();
}

#[cfg(not(all(target_os = "linux", feature = "uring")))]
fn main() {
    eprintln!("uring_throughput bench requires target_os=linux + feature=uring; skipping.");
}
