const std = @import("std");
pub fn main() !void {
    var v: [2000]i32 = undefined;
    for (0..2000) |i| v[i] = @intCast(i);
    std.debug.print("{d}\n", .{2000});
}
