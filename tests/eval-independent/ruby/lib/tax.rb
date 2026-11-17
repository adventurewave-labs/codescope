# frozen_string_literal: true

# Tax table module: module-method calls, not class instances.
module Tax
  TABLE = { 'EU' => 0.19, 'US' => 0.07 }.freeze

  def self.rate_for(zone)
    clamp(TABLE.fetch(zone, 0.2)) # @eval clamp=Tax::clamp@lib/tax.rb
  end

  def self.clamp(value)
    return 0.2 if value.respond_to?(:nan?) && value.nan? # @eval nan?=-

    value
  end
end
