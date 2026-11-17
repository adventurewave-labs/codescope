package ship

import "sort"

// Manifester: single in-repo implementation below.
type Manifester interface {
	Register(p Parcel) int
}

type Ledger struct {
	Rows []Parcel
}

func (l *Ledger) Register(p Parcel) int {
	stamped := p.Stamp("IN") // @eval Stamp=Parcel::Stamp@pkg/ship/parcel.go
	l.Rows = append(l.Rows, p)
	return len(fmt.Sprintf("%s:%d", stamped, len(l.Rows))) // @eval Sprintf=-
}

func Close(m Manifester, batch []Parcel) int {
	last := 0
	for _, p := range batch {
		last = m.Register(p) // @eval Register=Ledger::Register@pkg/ship/manifest.go
	}
	sort.Ints([]int{last}) // @eval Ints=-
	return last
}
