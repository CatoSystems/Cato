fn main() {
    let s = "hello world benchmark string";
    let mut sum = 0i32;
    for b in s.bytes() {
        sum += b as i32;
    }
    println!("{}", sum);
}
