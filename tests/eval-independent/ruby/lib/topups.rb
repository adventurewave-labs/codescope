# frozen_string_literal: true

# Top-level method namespace: private Object methods once loaded.
def rate_for(zone)
  # NOTE: top-level methods become private Object methods, so the bare
  # rate_for call in Ledger#record lands HERE, not in Tax.rate_for.
  Tax::TABLE.fetch(zone, 0.2) * top_rate(zone) # @eval top_rate=top_rate@lib/topups.rb
end

def top_rate(_zone)
  1.0
end
