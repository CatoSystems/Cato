const std = @import("std");

fn iter(x0: f64, y0: f64) i64 {
    var x: f64 = 0.0;
    var y: f64 = 0.0;
    var i: i64 = 0;
    while (i < 100) : (i += 1) {
        if (x * x + y * y > 4.0) return i;
        const nx = x * x - y * y + x0;
        const ny = 2.0 * x * y + y0;
        x = nx;
        y = ny;
    }
    return i;
}

pub fn main() void {
    var total: i64 = 0;
    var py: i64 = 0;
    while (py < 400) : (py += 1) {
        var px: i64 = 0;
        while (px < 400) : (px += 1) {
            total += iter(@as(f64, @floatFromInt(px)) / 200.0 - 2.0, @as(f64, @floatFromInt(py)) / 200.0 - 1.0);
        }
    }
    std.debug.print("{d}\n", .{total});
}
