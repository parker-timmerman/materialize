use mz_persist_client::internals_bench::state_diff;

fn main() {
    let mut buf = Vec::with_capacity(500 * 1024);
    let diff = state_diff();

    for _ in 0..10_000 {
        buf.clear();
        std::hint::black_box(diff.encode(&mut buf));
    }
}