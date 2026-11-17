# Ruby side: clean require of a real module.
require_relative 'support'

def run_rb(seed)
  Support.amplify(seed) # @eval amplify=Support::amplify@support.rb
end
