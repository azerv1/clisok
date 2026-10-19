use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("config-expansion");
    for count in [100, 1_000, 10_000] {
        let input = "${HOME}/bin $ ${MISSING:-fallback} ".repeat(count);
        group.bench_with_input(BenchmarkId::from_parameter(count), &input, |b, input| {
            b.iter(|| {
                clisok::parsing::expand(black_box(input), &|key| {
                    (key == "HOME").then(|| "/fixture".to_string())
                })
            })
        });
    }
    group.finish();
}
criterion_group!(benches, parsing);
criterion_main!(benches);
