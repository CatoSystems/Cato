const std = @import("std");

fn sumOnce(s: []const u8) i64 {
    var acc: i64 = 0;
    for (s) |b| {
        acc += @as(i64, b);
    }
    return acc;
}

pub fn main() void {
    const s = "the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnopqrstuvwxyz";
    var acc: i64 = 0;
    var k: i64 = 0;
    while (k < 20000000) : (k += 1) {
        acc += sumOnce(s);
    }
    std.debug.print("{d}\n", .{acc});
}
