package main

import "fmt"

type Tree struct {
	leaf bool
	v    int64
	l, r *Tree
}

func build(lo, hi int64) *Tree {
	if lo > hi {
		return &Tree{leaf: true}
	}
	mid := (lo + hi) / 2
	return &Tree{v: mid, l: build(lo, mid-1), r: build(mid+1, hi)}
}

func sum(t *Tree) int64 {
	if t.leaf {
		return 0
	}
	return t.v + sum(t.l) + sum(t.r)
}

func main() {
	fmt.Println(sum(build(0, 1000000)))
}
