package main

import "fmt"

func fibmod(n, m int64) int64 {
	var a, b, i int64 = 0, 1, 0
	for i < n {
		s := a + b
		a = b
		if s >= m {
			b = s - m
		} else {
			b = s
		}
		i++
	}
	return b
}

func main() {
	fmt.Println(fibmod(100000000, 1000000007))
}
