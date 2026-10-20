fn iter(x0: f64, y0: f64) -> i64 {
    let (mut x, mut y, mut i) = (0.0f64, 0.0f64, 0i64);
    while i < 100 {
        if x * x + y * y > 4.0 {
            return i;
        }
        let nx = x * x - y * y + x0;
        let ny = 2.0 * x * y + y0;
        x = nx;
        y = ny;
        i += 1;
    }
    i
}

fn main() {
    let mut total = 0i64;
    let mut py = 0i64;
    while py < 800 {
        let mut px = 0i64;
        while px < 800 {
            total += iter(px as f64 / 400.0 - 2.0, py as f64 / 400.0 - 1.0);
            px += 1;
        }
        py += 1;
    }
    println!("{}", total);
}
