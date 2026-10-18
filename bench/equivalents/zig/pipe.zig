const std = @import("std");

pub fn main() void {
    var s: i64 = 0;
    var i: i64 = 0;
    while (i < 3000000) : (i += 1) {
        if (@mod(i, 2) == 0) {
            s += i * i;
        }
    }
    std.debug.print("{d}\n", .{s});
}
