use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("python-entry");
    for count in [100, 1_000, 10_000] {
        let input = format!(
            "# {}\nimport sys\nfrom fixture.cli import main\nsys.exit(main())\n",
            "fixture ".repeat(count)
        );
        group.bench_with_input(BenchmarkId::from_parameter(count), &input, |b, input| {
            b.iter(|| clisok::parsing::entry_module(black_box(input)))
        });
    }
    group.finish();
}
criterion_group!(benches, parsing);
criterion_main!(benches);
