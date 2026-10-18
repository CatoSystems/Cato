fn fibmod(n: i64, m: i64) -> i64 {
    let (mut a, mut b, mut i) = (0i64, 1i64, 0i64);
    while i < n {
        let s = a + b;
        a = b;
        b = if s >= m { s - m } else { s };
        i += 1;
    }
    b
}

fn main() {
    println!("{}", fibmod(100_000_000, 1_000_000_007));
}
