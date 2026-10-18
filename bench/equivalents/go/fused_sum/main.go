package main

import "fmt"

func main() {
	var s int64 = 0
	for i := int64(0); i < 200000000; i++ {
		s += i
	}
	fmt.Println(s)
}
