import pathlib


class ConfigStore:
    def load(self, path: str) -> str:
        return parse_config(path)


def parse_config(path: str) -> str:
    return pathlib.Path(path).read_text()