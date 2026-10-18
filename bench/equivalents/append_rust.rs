fn main() {
    let mut v = Vec::with_capacity(0);
    for i in 0..2000 {
        v.push(i);
    }
    println!("{}", v.len());
}
