module Support
  def self.amplify(x)
    x * beta_factor # @eval beta_factor=Support::beta_factor@support.rb
  end

  def self.beta_factor
    3
  end
end
