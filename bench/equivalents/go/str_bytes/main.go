package main

import "fmt"

func sumOnce(s string) int64 {
	var acc int64 = 0
	for i := 0; i < len(s); i++ {
		acc += int64(s[i])
	}
	return acc
}

func main() {
	s := "the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnopqrstuvwxyz"
	var acc int64 = 0
	for k := 0; k < 20000000; k++ {
		acc += sumOnce(s)
	}
	fmt.Println(acc)
}
