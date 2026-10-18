const std = @import("std");

pub fn main() void {
    var s: i64 = 0;
    var i: i64 = 0;
    while (i < 200000000) : (i += 1) {
        s += i;
    }
    std.debug.print("{d}\n", .{s});
}
