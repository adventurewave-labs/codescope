export class UserService {
  load(id: string) {
    return this.fetch(id); // @eval fetch=UserService::fetch
  }
  fetch(id: string) {
    return id;
  }
}
export function format(u: string) {
  return u.trim(); // @eval trim=-
}
