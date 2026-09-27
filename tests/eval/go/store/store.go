package store

// Store persists records.
type Store struct{}

func NewStore() *Store { return &Store{} }

func (s *Store) Put(k string) {
	s.flush() // @eval flush=Store::flush
}

func (s *Store) flush() {}

func Open() {}
