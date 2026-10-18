const std = @import("std");

const Tree = struct {
    v: i64,
    l: ?*Tree,
    r: ?*Tree,
};

fn alloc() std.mem.Allocator {
    return std.heap.c_allocator;
}

fn build(lo: i64, hi: i64) *Tree {
    const a = alloc();
    if (lo > hi) {
        const leaf = a.create(Tree) catch unreachable;
        leaf.* = .{ .v = 0, .l = null, .r = null };
        return leaf;
    }
    const mid = @divTrunc(lo + hi, 2);
    const node = a.create(Tree) catch unreachable;
    node.* = .{ .v = mid, .l = build(lo, mid - 1), .r = build(mid + 1, hi) };
    return node;
}

fn sum(t: *Tree) i64 {
    var acc = t.v;
    if (t.l) |l| acc += sum(l);
    if (t.r) |r| acc += sum(r);
    return acc;
}

pub fn main() void {
    const root = build(0, 100000);
    std.debug.print("{d}\n", .{sum(root)});
}
