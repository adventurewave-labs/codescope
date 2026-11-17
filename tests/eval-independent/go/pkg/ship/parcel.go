package ship

import (
	"fmt"
	"strings"
)

// Parcel: pointer-receiver methods.
type Parcel struct {
	SKU    string
	Weight int
}

func (p *Parcel) Stamp(prefix string) string {
	return prefix + "-" + p.code() // @eval code=Parcel::code@pkg/ship/parcel.go
}

func (p *Parcel) code() string {
	return sanitize(p.SKU) // @eval sanitize=sanitize@pkg/ship/parcel.go
}

func sanitize(sku string) string {
	return strings.ToUpper(strings.ReplaceAll(sku, " ", "")) // @eval ToUpper=-
}
