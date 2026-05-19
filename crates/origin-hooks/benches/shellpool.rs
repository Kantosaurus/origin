use criterion::{criterion_group, criterion_main, Criterion};
use origin_hooks::{ShellPool, ShellSpec};

fn pool_dispatch(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let spec = if cfg!(windows) {
        ShellSpec {
            program: "cmd.exe".into(),
            args: vec!["/Q".into(), "/K".into(), "@echo off".into()],
            read_terminator: 0,
        }
    } else {
        ShellSpec {
            program: "/bin/sh".into(),
            args: vec!["-s".into()],
            read_terminator: 0,
        }
    };
    let pool = rt.block_on(async { ShellPool::new(spec, 2).await.expect("pool") });

    c.bench_function("shellpool/dispatch", |b| {
        b.iter(|| {
            rt.block_on(async {
                let script = if cfg!(windows) {
                    "echo x&<NUL set /p=\"\x00\"\r\n"
                } else {
                    "printf 'x\\0'\n"
                };
                let _ = pool.dispatch(script).await;
            });
        });
    });
}

criterion_group!(benches, pool_dispatch);
criterion_main!(benches);
