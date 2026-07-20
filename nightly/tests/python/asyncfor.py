# Taint produced by an async generator and consumed through `async for`. agen()
# yields a value derived from source(); the driving `async for` binds each item
# and sinks it. Exercises taint flow through `GET_ANEXT` and the async-iteration
# await protocol (SEND / END_SEND).
async def agen():
    yield source()


async def main():
    async for item in agen():
        sink(item)
