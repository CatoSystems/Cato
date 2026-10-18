fn main() {
    let mut s = 0i64;
    for i in 0..200_000_000 {
        s += i;
    }
    println!("{}", s);
}
