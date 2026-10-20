package main

import "fmt"

func main() {
	v := make([]int64, 0)
	for i := int64(0); i < 200000; i++ {
		v = append(v, i)
	}
	fmt.Println(len(v))
}
