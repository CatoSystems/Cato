package main

import "fmt"

func iter(x0, y0 float64) int64 {
	var x, y float64 = 0.0, 0.0
	var i int64 = 0
	for i < 100 {
		if x*x+y*y > 4.0 {
			return i
		}
		nx := x*x - y*y + x0
		ny := 2.0*x*y + y0
		x, y = nx, ny
		i++
	}
	return i
}

func main() {
	var total int64 = 0
	for py := int64(0); py < 400; py++ {
		for px := int64(0); px < 400; px++ {
			total += iter(float64(px)/200.0-2.0, float64(py)/200.0-1.0)
		}
	}
	fmt.Println(total)
}
