const std = @import("std");
pub fn main() !void {
    const s = "hello world benchmark string";
    var sum: i32 = 0;
    for (s) |b| sum += @as(i32, b);
    std.debug.print("{d}\n", .{sum});
}
