require_relative 'repo'

class Invoice
  def total
    tax_for(1) # @eval tax_for=Invoice::tax_for
    items.sum # @eval sum=-
  end

  def self.build
    r = Repo.new # @eval Repo=Repo
    r.save # @eval save=Repo::save
  end

  def tax_for(x)
    x
  end
end
