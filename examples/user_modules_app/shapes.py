from mathlib import mul


class Rect:
    def __init__(self, width: int, height: int):
        self.width = width
        self.height = height

    def area(self) -> int:
        return mul(self.width, self.height)
