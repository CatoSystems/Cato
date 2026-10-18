package main

import "fmt"

func main() {
	var s int64 = 0
	for i := int64(0); i < 3000000; i++ {
		if i%2 == 0 {
			s += i * i
		}
	}
	fmt.Println(s)
}
