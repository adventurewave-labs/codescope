class Cache:
    def save(self, item):
        self.validate(item)  # @eval validate=Cache::validate

    def validate(self, item):
        pass


def connect():
    pass
