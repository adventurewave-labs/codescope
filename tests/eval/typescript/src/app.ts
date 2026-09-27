import { UserService, format } from "./users";

export function main() {
  const svc = new UserService(); // @eval UserService=UserService
  svc.load("1"); // @eval load=UserService::load
  format("x"); // @eval format=format@src/users.ts
  console.log("hi"); // @eval log=-
  const xs = [1, 2].map((x) => x); // @eval map=-
  return run(); // @eval run=run
}

function run() {
  return 1;
}

export class Controller {
  constructor(private users: UserService) {}
  show() {
    return this.users.load("1"); // @eval load=UserService::load
  }
}
