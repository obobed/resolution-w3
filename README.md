# Hastebin
A light and simple pastebin clone, with pastes that anyone can see :D

## Documentation
| method | endpoint | description |
| ------ | -------- | ----------- |
| `GET`  | `/health`| Check API status |
| `POST`  | `/pastes/new` | Create a new paste |
| `GET`  | `/pastes/{id}` | Get a specific paste by id |
| `GET`  | `/pastes` | List the 50 most recent pastes |

### Paste POST schema
| field | type | description |
| ----- | ---- | ----------- |
| `content` | String | The text you would like to store. Max 3200 characters |