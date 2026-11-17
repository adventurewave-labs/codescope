# frozen_string_literal: true

require_relative 'tax'
require_relative 'topups'

# Invoice ledger.
class Ledger
  attr_reader :entries

  def initialize
    @entries = []
  end

  def record(amount, zone)
    # Bare rate_for: Ledger has no such method, so Ruby lookup reaches the
    # top-level definition in topups.rb (Object private method), NOT Tax.
    rate = rate_for(zone) # @eval rate_for=rate_for@lib/topups.rb
    @entries.push(amount: amount, total: amount * rate) # @eval push=-
    @entries.size
  end

  def surcharge(zone)
    Tax.rate_for(zone) > 0.15 ? 5 : 0 # @eval rate_for=Tax::rate_for@lib/tax.rb
  end

  def total
    @entries.sum { |e| e[:total] } # @eval sum=-
  end
end
