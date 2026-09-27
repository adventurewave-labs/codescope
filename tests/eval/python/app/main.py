from app.repo import Repo, connect
import json


def run():
    r = Repo()  # @eval Repo=Repo
    r.save(1)  # @eval save=Repo::save
    connect()  # @eval connect=connect@app/repo.py
    json.dumps({})  # @eval dumps=-
    helper()  # @eval helper=helper
    print("x")  # @eval print=-


def helper():
    pass


class Service:
    def __init__(self):
        self.repo = Repo()  # @eval Repo=Repo

    def handle(self):
        self.repo.save(1)  # @eval save=Repo::save
