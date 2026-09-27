class Repo:
    def save(self, item):
        self.validate(item)  # @eval validate=Repo::validate
        self.items.append(item)  # @eval append=-

    def validate(self, item):
        pass


def connect():
    pass
