const std = @import("std");

pub fn main() void {
    const a = std.heap.c_allocator;
    var v: std.ArrayList(i64) = .empty;
    var i: i64 = 0;
    while (i < 20000) : (i += 1) {
        v.append(a, i) catch unreachable;
    }
    std.debug.print("{d}\n", .{v.items.len});
}
