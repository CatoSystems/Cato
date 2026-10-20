fn main() {
    let mut v: Vec<i64> = Vec::new();
    for i in 0..200000 {
        v.push(i);
    }
    println!("{}", v.len());
}
