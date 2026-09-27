class Repo
  def save
    flush # @eval flush=Repo::flush
    puts 'saved' # @eval puts=-
  end

  def flush
  end
end

class Cart
  def save
  end
end
