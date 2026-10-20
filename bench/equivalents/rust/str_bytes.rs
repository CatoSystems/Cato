fn sum_once(s: &[u8]) -> i64 {
    let mut acc = 0i64;
    for &b in s {
        acc += b as i64;
    }
    acc
}

fn main() {
    let s = b"the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnopqrstuvwxyz";
    let mut acc = 0i64;
    let mut k = 0i64;
    while k < 20_000_000 {
        acc += sum_once(s);
        k += 1;
    }
    println!("{}", acc);
}
