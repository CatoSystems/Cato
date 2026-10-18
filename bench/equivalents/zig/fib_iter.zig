const std = @import("std");

fn fibmod(n: i64, m: i64) i64 {
    var a: i64 = 0;
    var b: i64 = 1;
    var i: i64 = 0;
    while (i < n) : (i += 1) {
        const s = a + b;
        a = b;
        b = if (s >= m) s - m else s;
    }
    return b;
}

pub fn main() void {
    std.debug.print("{d}\n", .{fibmod(100000000, 1000000007)});
}
