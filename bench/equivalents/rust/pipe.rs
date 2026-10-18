fn main() {
    let mut s = 0i64;
    for i in 0..3_000_000 {
        if i % 2 == 0 {
            s += i * i;
        }
    }
    println!("{}", s);
}
